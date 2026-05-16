use crate::ast::{Annotation, Node, NodeState, Plan};
use thiserror::Error;
use winnow::Parser;
use winnow::ascii::space0;
use winnow::combinator::{alt, delimited, opt};
use winnow::error::ContextError;
use winnow::token::{rest, take_while};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("line {line}: unrecognized checkbox state `[{state}]`, expected `[ ]` or `[x]`")]
    BadCheckboxState { line: usize, state: String },
    #[error("line {line}: unterminated fenced code block opened at line {opened_at}")]
    UnterminatedCodeFence { line: usize, opened_at: usize },
}

pub fn parse(input: &str) -> Result<Plan, ParseError> {
    let mut plan = Plan::default();
    let mut stack: Vec<(Node, usize)> = Vec::new(); // (node, indent)
    let mut preamble: Vec<String> = Vec::new();
    let mut saw_checkbox = false;
    let mut in_code: Option<CodeAccumulator> = None;

    for (idx, raw_line) in input.lines().enumerate() {
        let line_no = idx + 1;

        if let Some(cs) = in_code.as_mut() {
            let trimmed = raw_line.trim_start();
            if trimmed.starts_with("```") {
                let code = in_code.take().expect("code state");
                let annotation = Annotation::CodeBlock {
                    lang: code.lang,
                    content: code.content,
                    indent: code.indent,
                };
                attach_annotation(&mut stack, &mut preamble, saw_checkbox, annotation);
            } else {
                cs.content.push_str(raw_line);
                cs.content.push('\n');
            }
            continue;
        }

        let indent = leading_spaces(raw_line);
        let trimmed = &raw_line[indent..];

        // Open fence
        if let Some(after) = trimmed.strip_prefix("```") {
            let lang = after.trim();
            let lang = if lang.is_empty() { None } else { Some(lang.to_string()) };
            in_code = Some(CodeAccumulator {
                indent,
                lang,
                content: String::new(),
                opened_at: line_no,
            });
            continue;
        }

        match parse_checkbox(trimmed, line_no)? {
            CheckboxLine::Checkbox { state, id, title } => {
                saw_checkbox = true;
                let node = Node {
                    id,
                    title,
                    state,
                    children: Vec::new(),
                    annotations: Vec::new(),
                };
                // Pop deeper-or-equal nodes off the stack (they're complete).
                while let Some((_, top_indent)) = stack.last() {
                    if *top_indent >= indent {
                        let (done, _) = stack.pop().unwrap();
                        push_into_parent(&mut plan, &mut stack, done);
                    } else {
                        break;
                    }
                }
                stack.push((node, indent));
            }
            CheckboxLine::NotACheckbox => {
                if raw_line.trim().is_empty() {
                    // Preserve blank lines in preamble verbatim; ignore inside the tree.
                    if !saw_checkbox {
                        preamble.push(raw_line.to_string());
                    }
                    continue;
                }
                let annotation = classify_annotation(raw_line, indent, trimmed);
                attach_annotation(&mut stack, &mut preamble, saw_checkbox, annotation);
            }
        }
    }

    if let Some(cs) = in_code {
        return Err(ParseError::UnterminatedCodeFence {
            line: input.lines().count(),
            opened_at: cs.opened_at,
        });
    }

    // Drain the stack — anything remaining is complete.
    while let Some((node, _)) = stack.pop() {
        push_into_parent(&mut plan, &mut stack, node);
    }

    plan.preamble = preamble;
    Ok(plan)
}

fn push_into_parent(plan: &mut Plan, stack: &mut Vec<(Node, usize)>, node: Node) {
    if let Some((parent, _)) = stack.last_mut() {
        parent.children.push(node);
    } else {
        plan.phases.push(node);
    }
}

fn attach_annotation(
    stack: &mut [(Node, usize)],
    preamble: &mut Vec<String>,
    saw_checkbox: bool,
    annotation: Annotation,
) {
    if !saw_checkbox {
        // Pre-checkbox annotations become preamble lines (re-serialized as-is).
        match annotation {
            Annotation::Text { text, indent } => {
                preamble.push(format!("{}{}", " ".repeat(indent), text));
            }
            Annotation::Bullet { text, indent } => {
                preamble.push(format!("{}- {}", " ".repeat(indent), text));
            }
            Annotation::CodeBlock {
                lang,
                content,
                indent,
            } => {
                let pad = " ".repeat(indent);
                let fence = match &lang {
                    Some(l) => format!("{pad}```{l}"),
                    None => format!("{pad}```"),
                };
                preamble.push(fence);
                for line in content.lines() {
                    preamble.push(line.to_string());
                }
                preamble.push(format!("{pad}```"));
            }
        }
        return;
    }
    if let Some((top, _)) = stack.last_mut() {
        top.annotations.push(annotation);
    } else {
        // After checkboxes started but no open node — shouldn't happen with a balanced tree.
        // Skip silently in v1.
    }
}

fn leading_spaces(s: &str) -> usize {
    s.chars().take_while(|c| *c == ' ').count()
}

enum CheckboxLine {
    Checkbox {
        state: NodeState,
        id: String,
        title: String,
    },
    NotACheckbox,
}

fn parse_checkbox(trimmed: &str, line_no: usize) -> Result<CheckboxLine, ParseError> {
    let Some(after_dash) = trimmed.strip_prefix("- ") else {
        return Ok(CheckboxLine::NotACheckbox);
    };
    let Some(after_open) = after_dash.strip_prefix('[') else {
        return Ok(CheckboxLine::NotACheckbox);
    };
    let Some((state, rest)) = after_open.split_once("] ") else {
        return Ok(CheckboxLine::NotACheckbox);
    };
    let state = match state {
        " " => NodeState::Pending,
        "x" | "X" => NodeState::Done,
        "-" | "~" => NodeState::WontDo,
        other => {
            return Err(ParseError::BadCheckboxState {
                line: line_no,
                state: other.to_string(),
            });
        }
    };

    let (id, title) = extract_id_and_title(rest);
    Ok(CheckboxLine::Checkbox { state, id, title })
}

/// Pull an optional id off the front of the post-checkbox text.
///
/// Accepts:
/// - bold-wrapped ids:  `**X.4.a.1** — title`
/// - bare ids:          `X.4.a.1 title` or `1.0 title`
/// - no id:             `do the thing` (returns `("", "do the thing")`)
///
/// Implemented as a winnow combinator: leading whitespace, then an `opt(alt(bold, bare))`
/// (so an unrecognized leading token gracefully degrades to no-id), then an optional
/// em-dash / hyphen / whitespace separator, then everything else as title. Always succeeds.
fn extract_id_and_title(input: &str) -> (String, String) {
    let mut input = input;
    id_and_title
        .parse_next(&mut input)
        .expect("id_and_title is total")
}

fn id_and_title(input: &mut &str) -> winnow::ModalResult<(String, String), ContextError> {
    space0.parse_next(input)?;
    let id = opt(alt((bold_id, bare_id)))
        .parse_next(input)?
        .unwrap_or_default();
    if !id.is_empty() {
        skip_separator.parse_next(input)?;
    }
    let title = rest.parse_next(input)?.trim_end().to_string();
    Ok((id, title))
}

fn bold_id(input: &mut &str) -> winnow::ModalResult<String, ContextError> {
    delimited("**", id_chars, "**").parse_next(input)
}

fn bare_id(input: &mut &str) -> winnow::ModalResult<String, ContextError> {
    id_chars.parse_next(input)
}

fn id_chars(input: &mut &str) -> winnow::ModalResult<String, ContextError> {
    take_while(1.., |c: char| c.is_ascii_alphanumeric() || c == '.')
        .verify(is_valid_id)
        .map(String::from)
        .parse_next(input)
}

fn skip_separator(input: &mut &str) -> winnow::ModalResult<(), ContextError> {
    let _: &str = take_while(0.., |c: char| {
        c == '—' || c == '-' || c.is_whitespace()
    })
    .parse_next(input)?;
    Ok(())
}

fn is_valid_id(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
        return false;
    }
    let has_dot = s.contains('.');
    let all_digits = s.chars().all(|c| c.is_ascii_digit());
    has_dot || all_digits
}

fn classify_annotation(raw: &str, indent: usize, trimmed: &str) -> Annotation {
    if let Some(rest) = trimmed.strip_prefix("- ") {
        Annotation::Bullet {
            text: rest.to_string(),
            indent,
        }
    } else {
        Annotation::Text {
            text: raw[indent..].to_string(),
            indent,
        }
    }
}

struct CodeAccumulator {
    indent: usize,
    lang: Option<String>,
    content: String,
    opened_at: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_empty_input() {
        let plan = parse("").unwrap();
        assert!(plan.preamble.is_empty());
        assert!(plan.phases.is_empty());
    }

    #[test]
    fn parses_single_phase_no_children() {
        let input = "- [ ] 1.0 First phase\n";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases.len(), 1);
        let phase = &plan.phases[0];
        assert_eq!(phase.id, "1.0");
        assert_eq!(phase.title, "First phase");
        assert!(!phase.is_done());
        assert!(phase.children.is_empty());
    }

    #[test]
    fn parses_completed_checkbox() {
        let plan = parse("- [x] 1.0 Done phase\n").unwrap();
        assert!(plan.phases[0].is_done());
    }

    #[test]
    fn parses_wont_do_checkbox() {
        let plan = parse("- [-] 1.0 Skipped phase\n").unwrap();
        assert_eq!(plan.phases[0].state, NodeState::WontDo);
        assert!(!plan.phases[0].is_done());
        assert!(plan.phases[0].is_resolved());
    }

    #[test]
    fn parses_tilde_as_wont_do_alias() {
        let plan = parse("- [~] 1.0 Skipped via tilde\n").unwrap();
        assert_eq!(plan.phases[0].state, NodeState::WontDo);
    }

    #[test]
    fn parses_nested_three_levels() {
        let input = "\
- [ ] 1.0 Phase
  - [ ] 1.1 Task
    - [ ] 1.1.1 Subtask
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases.len(), 1);
        let phase = &plan.phases[0];
        assert_eq!(phase.children.len(), 1);
        let task = &phase.children[0];
        assert_eq!(task.id, "1.1");
        assert_eq!(task.children.len(), 1);
        assert_eq!(task.children[0].id, "1.1.1");
    }

    #[test]
    fn handles_four_space_indent() {
        let input = "\
- [ ] 1.0 Phase
    - [ ] 1.1 Task
        - [ ] 1.1.1 Subtask
";
        let plan = parse(input).unwrap();
        let phase = &plan.phases[0];
        assert_eq!(phase.children.len(), 1, "1.1 should be child of 1.0");
        assert_eq!(phase.children[0].children.len(), 1, "1.1.1 should be child of 1.1");
    }

    #[test]
    fn handles_mixed_indent_across_phases() {
        let input = "\
- [ ] 1.0 P1
  - [ ] 1.1 T
- [ ] 2.0 P2
    - [ ] 2.1 T
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases.len(), 2);
        assert_eq!(plan.phases[0].children.len(), 1);
        assert_eq!(plan.phases[1].children.len(), 1);
    }

    #[test]
    fn captures_preamble() {
        let input = "\
# Header

Some prose.

- [ ] 1.0 Phase
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.preamble.len(), 4); // header, blank, prose, blank
        assert_eq!(plan.preamble[0], "# Header");
        assert_eq!(plan.phases.len(), 1);
    }

    #[test]
    fn attaches_bullet_annotation_to_node() {
        let input = "\
- [ ] 1.0 Phase
  - note as bullet
  - [ ] 1.1 Task
";
        let plan = parse(input).unwrap();
        let phase = &plan.phases[0];
        assert_eq!(phase.annotations.len(), 1);
        match &phase.annotations[0] {
            Annotation::Bullet { text, indent } => {
                assert_eq!(text, "note as bullet");
                assert_eq!(*indent, 2);
            }
            other => panic!("expected Bullet, got {other:?}"),
        }
        assert_eq!(phase.children.len(), 1);
    }

    #[test]
    fn attaches_text_annotation_to_node() {
        let input = "\
- [ ] 1.0 Phase
  Some context for the phase.
";
        let plan = parse(input).unwrap();
        let phase = &plan.phases[0];
        assert_eq!(phase.annotations.len(), 1);
        match &phase.annotations[0] {
            Annotation::Text { text, indent } => {
                assert_eq!(text, "Some context for the phase.");
                assert_eq!(*indent, 2);
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn attaches_code_block_annotation() {
        let input = "\
- [ ] 1.0 Phase
  ```rust
  fn foo() {}
  ```
";
        let plan = parse(input).unwrap();
        let phase = &plan.phases[0];
        assert_eq!(phase.annotations.len(), 1);
        match &phase.annotations[0] {
            Annotation::CodeBlock { lang, content, indent } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert!(content.contains("fn foo()"));
                assert_eq!(*indent, 2);
            }
            other => panic!("expected CodeBlock, got {other:?}"),
        }
    }

    #[test]
    fn tolerates_checkbox_without_id() {
        let plan = parse("- [ ] no id here\n").unwrap();
        assert_eq!(plan.phases[0].id, "");
        assert_eq!(plan.phases[0].title, "no id here");
    }

    #[test]
    fn parses_bold_wrapped_id() {
        let plan = parse("- [x] **X.4.a.1** — Studio severability test\n").unwrap();
        assert_eq!(plan.phases[0].id, "X.4.a.1");
        assert_eq!(plan.phases[0].title, "Studio severability test");
        assert!(plan.phases[0].is_done());
    }

    #[test]
    fn parses_alphanumeric_id_without_bold() {
        let plan = parse("- [ ] X.4.a.1 title here\n").unwrap();
        assert_eq!(plan.phases[0].id, "X.4.a.1");
        assert_eq!(plan.phases[0].title, "title here");
    }

    #[test]
    fn parses_em_dash_separator() {
        let plan = parse("- [ ] 1.0 — title with em-dash\n").unwrap();
        assert_eq!(plan.phases[0].id, "1.0");
        assert_eq!(plan.phases[0].title, "title with em-dash");
    }

    #[test]
    fn parses_hyphen_separator() {
        let plan = parse("- [ ] 1.0 - title with hyphen\n").unwrap();
        assert_eq!(plan.phases[0].id, "1.0");
        assert_eq!(plan.phases[0].title, "title with hyphen");
    }

    #[test]
    fn does_not_treat_first_title_word_as_id() {
        // "Make" has no dot and isn't all-digits, so it must NOT be grabbed as an id.
        let plan = parse("- [ ] Make the core domain model the source\n").unwrap();
        assert_eq!(plan.phases[0].id, "");
        assert_eq!(plan.phases[0].title, "Make the core domain model the source");
    }

    #[test]
    fn section_header_inside_tree_attaches_as_text_annotation() {
        let input = "\
- [ ] 1.0 Phase
  ## A markdown heading
  - [ ] 1.1 Task
";
        let plan = parse(input).unwrap();
        let phase = &plan.phases[0];
        assert_eq!(phase.annotations.len(), 1);
        match &phase.annotations[0] {
            Annotation::Text { text, .. } => assert_eq!(text, "## A markdown heading"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert_eq!(phase.children.len(), 1, "1.1 should still be a child of 1.0");
    }

    #[test]
    fn smoke_test_quicksight_plan_md() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let path = manifest_dir.parent().unwrap().join("quicksight/PLAN.md");
        if !path.exists() {
            // The neighboring quicksight repo is the user's real-world test target.
            // If it's not checked out, skip silently — this test is opportunistic.
            return;
        }
        let input = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let plan = parse(&input)
            .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        assert!(!plan.phases.is_empty(), "quicksight PLAN.md should have phases");
    }

    #[test]
    fn rejects_bad_checkbox_state() {
        let err = parse("- [?] 1.0 weird\n").unwrap_err();
        assert!(matches!(err, ParseError::BadCheckboxState { line: 1, .. }));
    }

    #[test]
    fn rejects_unterminated_code_fence() {
        let input = "\
- [ ] 1.0 Phase
  ```rust
  fn foo() {}
";
        let err = parse(input).unwrap_err();
        assert!(matches!(err, ParseError::UnterminatedCodeFence { .. }));
    }

    #[test]
    fn tolerates_short_id_form() {
        let input = "\
- [ ] 1 Phase
  - [ ] 1.1 Task
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases[0].id, "1");
        assert_eq!(plan.phases[0].children[0].id, "1.1");
    }

    #[test]
    fn parses_basic_fixture() {
        let input = include_str!("../tests/fixtures/basic.md");
        let plan = parse(input).unwrap();

        // Preamble: title + blank + prose + blank = 4 lines.
        assert_eq!(plan.preamble.len(), 4);
        assert_eq!(plan.preamble[0], "# Test fixture");

        // Two top-level phases.
        assert_eq!(plan.phases.len(), 2);

        let p1 = &plan.phases[0];
        assert_eq!(p1.id, "1.0");
        assert!(!p1.is_done());
        assert_eq!(p1.children.len(), 2);

        let t11 = &p1.children[0];
        assert_eq!(t11.id, "1.1");
        assert_eq!(t11.children.len(), 2);
        assert!(t11.children[0].is_done(), "1.1.1 should be checked");
        assert!(!t11.children[1].is_done(), "1.1.2 should be unchecked");

        let t12 = &p1.children[1];
        assert_eq!(t12.id, "1.2");
        assert_eq!(t12.annotations.len(), 2, "1.2 has text + bullet annotations");

        let p2 = &plan.phases[1];
        assert_eq!(p2.id, "2.0");
        assert!(p2.is_done());
        assert_eq!(p2.children.len(), 1);
        assert!(p2.children[0].is_done());
    }

    #[test]
    fn parses_this_repos_plan_md() {
        // e2e smoke test: the live PLAN.md in this repo should always parse.
        // It will drift as work progresses; we only check it parses and has phases.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("PLAN.md");
        let input = std::fs::read_to_string(&path).expect("PLAN.md exists");
        let plan = parse(&input).expect("PLAN.md must parse cleanly");
        assert!(!plan.phases.is_empty(), "PLAN.md should declare phases");
    }

    #[test]
    fn ignores_blank_lines_inside_tree() {
        let input = "\
- [ ] 1.0 Phase

  - [ ] 1.1 Task

  - [ ] 1.2 Other
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases[0].children.len(), 2);
    }
}

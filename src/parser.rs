use crate::ast::{Annotation, Node, NodeState, Phase, Plan};
use thiserror::Error;
use winnow::Parser;
use winnow::ascii::space0;
use winnow::combinator::opt;
use winnow::error::ContextError;
use winnow::token::{rest, take_while};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("line {line}: unrecognized checkbox state `[{state}]`, expected `[ ]` or `[x]`")]
    BadCheckboxState { line: usize, state: String },
    #[error("line {line}: unterminated fenced code block opened at line {opened_at}")]
    UnterminatedCodeFence { line: usize, opened_at: usize },
    #[error(
        "column-0 checkbox `- [ ] {id} {title}` has no `## Phase` header above it. \
         FORMATv2 (v1.0.0+) requires every task to live under a `## Phase X - Title` \
         header; the legacy `- [ ] N.0` anchor form is no longer supported. \
         Run `claude-plan-bridge canonicalize` on v0.9 to migrate, then upgrade."
    )]
    OrphanCheckbox { id: String, title: String },
}

pub fn parse(input: &str) -> Result<Plan, ParseError> {
    let mut plan = Plan::default();
    let mut stack: Vec<(Node, usize)> = Vec::new(); // (node, indent)
    let mut preamble: Vec<String> = Vec::new();
    let mut saw_checkbox = false;
    let mut in_code: Option<CodeAccumulator> = None;
    // Phase 36.3: FORMATv2 phase headers (`## Phase X - Title *(depends on: Y)*`)
    // open a new Phase here. Subsequent top-level checkboxes land in
    // `current_phase.children` instead of becoming their own Phases. A new
    // header (or EOF) finalizes the current phase and pushes it onto
    // `plan.phases`. Legacy v1 checkbox anchors that appear BEFORE any header
    // keep promoting to Phase via the existing `Phase::from_node` path.
    let mut current_phase: Option<Phase> = None;

    // Phase 35.1a: peel a *trailing* `## Backlog (not yet phased)` block off the
    // tail before the tree walk sees it. Without this, the bridge's own
    // serialized output (Backlog rendered last) would re-parse as annotations
    // dangling off the final checkbox node — and the next phase-append would
    // slip ahead of it. Lifting it into `plan.backlog` keeps the section
    // pinned to the bottom across edits. Only a genuinely trailing block is
    // lifted; a preamble or mid-document Backlog stays where it is until
    // `consolidate_backlog` (canonicalize / explicit backlog ops) moves it.
    let (body, trailing_backlog, backlog_was_h1) = peel_trailing_backlog(input);
    plan.backlog = trailing_backlog;
    plan.backlog_h1 = backlog_was_h1;

    for (idx, raw_line) in body.iter().enumerate() {
        let raw_line = *raw_line;
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
                attach_annotation(
                    &mut stack,
                    &mut preamble,
                    &mut current_phase,
                    saw_checkbox,
                    annotation,
                );
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
            let lang = if lang.is_empty() {
                None
            } else {
                Some(lang.to_string())
            };
            in_code = Some(CodeAccumulator {
                indent,
                lang,
                content: String::new(),
                opened_at: line_no,
            });
            continue;
        }

        // Phase 36.3: FORMATv2 phase header detection. A column-0
        // `## Phase <ID> - <Title> *(depends on: ...)*  *(prefer after: ...)*`
        // line drains the stack, finalizes any previous open phase, and opens
        // a fresh one. Markers are independent — either, both, or neither may
        // appear; titles ending right at the newline (no markers) work too.
        if indent == 0
            && let Some(header) = parse_v2_phase_header(raw_line)
        {
            while let Some((node, _)) = stack.pop() {
                push_into_parent(&mut stack, &mut current_phase, node)?;
            }
            if let Some(p) = current_phase.take() {
                plan.phases.push(p);
            }
            current_phase = Some(Phase {
                // Phases are bare ids under FORMATv2. Strip a trailing `.0`
                // left over from an old `## Phase 42.0` so in-memory phase ids
                // are always bare — the header re-emits as `## Phase 42`.
                id: bare_phase_id(&header.id),
                title: header.title,
                state: NodeState::Pending,
                children: vec![],
                annotations: vec![],
                depends_on: header.depends_on,
                prefer_after: header.prefer_after,
            });
            // Opening a phase header counts as entering the tree — subsequent
            // blank lines coalesce as Annotation::Blank instead of preamble.
            saw_checkbox = true;
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
                        push_into_parent(&mut stack, &mut current_phase, done)?;
                    } else {
                        break;
                    }
                }
                stack.push((node, indent));
            }
            CheckboxLine::NotACheckbox => {
                if raw_line.trim().is_empty() {
                    if !saw_checkbox {
                        // Preamble keeps blank lines verbatim.
                        preamble.push(raw_line.to_string());
                        continue;
                    }
                    // Phase 29.6: inside the tree, capture blank lines as a
                    // single `Annotation::Blank { count }`. Coalesce
                    // consecutive blanks so a row of 3 blank lines round-trips
                    // as one annotation with count=3.
                    bump_blank_on_top(&mut stack, &mut current_phase);
                    continue;
                }
                let annotation = classify_annotation(raw_line, indent, trimmed);
                attach_annotation(
                    &mut stack,
                    &mut preamble,
                    &mut current_phase,
                    saw_checkbox,
                    annotation,
                );
            }
        }
    }

    if let Some(cs) = in_code {
        return Err(ParseError::UnterminatedCodeFence {
            line: body.len(),
            opened_at: cs.opened_at,
        });
    }

    // Drain the stack — anything remaining is complete.
    while let Some((node, _)) = stack.pop() {
        push_into_parent(&mut stack, &mut current_phase, node)?;
    }
    if let Some(p) = current_phase.take() {
        plan.phases.push(p);
    }

    plan.preamble = preamble;
    Ok(plan)
}

fn push_into_parent(
    stack: &mut [(Node, usize)],
    current_phase: &mut Option<Phase>,
    node: Node,
) -> Result<(), ParseError> {
    if let Some((parent, _)) = stack.last_mut() {
        parent.children.push(node);
    } else if let Some(phase) = current_phase.as_mut() {
        // Inside a FORMATv2 phase: top-level checkboxes become its tasks.
        phase.children.push(node);
    } else {
        // No header context — a column-0 checkbox before any `## Phase`
        // header. The legacy `- [ ] N.0` anchor that v1 promoted to a phase
        // is gone in FORMATv2; refuse with a migration hint rather than
        // silently inventing structure.
        return Err(ParseError::OrphanCheckbox {
            id: node.id,
            title: node.title,
        });
    }
    Ok(())
}

/// Parsed FORMATv2 phase header (`## Phase <ID> - <Title>` with optional
/// `*(depends on: ...)*` and/or `*(prefer after: ...)*` markers).
struct PhaseHeader {
    id: String,
    title: String,
    depends_on: Vec<String>,
    prefer_after: Vec<String>,
}

/// Phase 42.3: normalize a phase id to its bare FORMATv2 form by dropping a
/// trailing `.0` left over from the legacy `X.0` convention. Bare ids and
/// dotted phase numbers (`3.5`) pass through unchanged.
fn bare_phase_id(id: &str) -> String {
    id.strip_suffix(".0").unwrap_or(id).to_string()
}

/// Match a FORMATv2 phase header line. Returns None for any line that isn't
/// the exact shape `## Phase <alphanumeric-ID><sep><title>` at column 0,
/// including `## Backlog`, `### Phase`, indented headers, headers without
/// the `Phase ` keyword, and bare `## Notes` style sections.
///
/// Separator tolerance on read (canonical write is ` - `): ` - ` hyphen or a
/// plain space following the ID — same liberality as checkbox lines. Em-dash
/// is a dropped v1 separator and is no longer recognized.
fn parse_v2_phase_header(line: &str) -> Option<PhaseHeader> {
    // Column-0 only. An indented `## Phase X` is markdown content inside a
    // task, not a phase boundary.
    if line.starts_with(' ') || line.starts_with('\t') {
        return None;
    }
    let after_hashes = line.strip_prefix("## ")?;
    let after_phase_keyword = after_hashes.strip_prefix("Phase ")?;

    // Extract the ID (alphanumeric, may include `.` for an edge `42.0`).
    let id_end = after_phase_keyword
        .find(|c: char| c.is_whitespace() || c == '-' || c == '*')
        .unwrap_or(after_phase_keyword.len());
    let id_part = after_phase_keyword[..id_end].trim();
    if id_part.is_empty() {
        return None;
    }
    if !id_part
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.')
    {
        return None;
    }
    let rest = after_phase_keyword[id_end..].trim_start();

    // Strip the separator (` - ` or bare whitespace). The rest may still start
    // with `-` from the separator itself.
    let after_sep = rest.trim_start_matches('-').trim_start();

    // Title runs until the first `*(` marker or end-of-line.
    let (title_raw, markers_part) = match after_sep.find("*(") {
        Some(i) => (&after_sep[..i], &after_sep[i..]),
        None => (after_sep, ""),
    };
    let title = title_raw.trim().to_string();

    let (depends_on, prefer_after) = parse_v2_phase_markers(markers_part);

    Some(PhaseHeader {
        id: id_part.to_string(),
        title,
        depends_on,
        prefer_after,
    })
}

/// Scan a tail like `*(depends on: AR)* *(prefer after: AB, AC)*` and
/// extract the two ID lists. Either marker can appear in either order, both
/// optional. Unknown marker bodies (e.g. `*(blocked by: X)*`) are ignored
/// silently — keeps the parser forward-compatible if FORMATv2 grows.
fn parse_v2_phase_markers(s: &str) -> (Vec<String>, Vec<String>) {
    let mut depends_on: Vec<String> = Vec::new();
    let mut prefer_after: Vec<String> = Vec::new();
    let mut s = s;
    while let Some(start) = s.find("*(") {
        s = &s[start + 2..];
        let Some(end) = s.find(")*") else {
            break;
        };
        let body = s[..end].trim();
        if let Some(list) = body.strip_prefix("depends on:") {
            depends_on.extend(split_id_list(list));
        } else if let Some(list) = body.strip_prefix("prefer after:") {
            prefer_after.extend(split_id_list(list));
        }
        s = &s[end + 2..];
    }
    (depends_on, prefer_after)
}

fn split_id_list(list: &str) -> impl Iterator<Item = String> + '_ {
    list.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Split `input` into (body lines, trailing-backlog body). When the document
/// *ends* with a `## Backlog (not yet phased)` section — heading followed only
/// by bullet/blank/continuation lines to EOF — the heading-and-below is peeled
/// off and the bullet lines (leading/trailing blanks trimmed) are returned as
/// the backlog body. The serializer re-emits the heading, so it's not stored.
///
/// Conservative by construction: the upward scan stops at the first line that
/// isn't blank, a `-` bullet, or an indented continuation. If that stopping
/// line isn't the Backlog heading (e.g. it's `## Sustainment`, a `### Backlog`
/// subsection, prose, or a checkbox), nothing is peeled — the Backlog stays in
/// the body for the tree walk / `consolidate_backlog` to handle.
fn peel_trailing_backlog(input: &str) -> (Vec<&str>, Vec<String>, bool) {
    let lines: Vec<&str> = input.lines().collect();

    // Walk up from EOF over backlog-body-shaped lines, looking for the heading.
    let mut i = lines.len();
    while i > 0 {
        let line = lines[i - 1];
        let trimmed = line.trim_start();
        // A checkbox line ends the scan — backlog bullets are never checkboxes,
        // so a `- [ ] ...` below the heading means this isn't a trailing block
        // (the Backlog sits above real phases and belongs to the preamble).
        let is_checkbox = trimmed.starts_with("- [");
        let is_body_shaped = !is_checkbox
            && (trimmed.is_empty()
                || trimmed.starts_with('-')
                || (line.starts_with(char::is_whitespace) && !trimmed.is_empty()));
        if crate::ast::is_backlog_heading(line) {
            // Detect heading level for round-trip: h1 `# Backlog` (FORMATv2)
            // vs h2 `## Backlog` (legacy). Both are accepted on read; the
            // serializer honors `plan.backlog_h1`. `# B...` starts with `# `
            // exactly, while `## B...` starts with `## ` (second char is `#`
            // not space), so prefix-match on `# ` AND NOT `## ` distinguishes.
            let was_h1 = trimmed.starts_with("# ") && !trimmed.starts_with("## ");
            let body_lines: Vec<String> =
                lines[i..].iter().map(|s| s.to_string()).collect::<Vec<_>>();
            let backlog = trim_blank_edges(body_lines);
            if backlog.is_empty() {
                return (lines, Vec::new(), false);
            }
            let mut kept: Vec<&str> = lines[..i - 1].to_vec();
            while kept.last().is_some_and(|l| l.trim().is_empty()) {
                kept.pop();
            }
            return (kept, backlog, was_h1);
        }
        if !is_body_shaped {
            break;
        }
        i -= 1;
    }
    (lines, Vec::new(), false)
}

fn trim_blank_edges(mut lines: Vec<String>) -> Vec<String> {
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines
}

fn attach_annotation(
    stack: &mut [(Node, usize)],
    preamble: &mut Vec<String>,
    current_phase: &mut Option<Phase>,
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
            Annotation::Blank { count } => {
                for _ in 0..count {
                    preamble.push(String::new());
                }
            }
        }
        return;
    }
    // Phase 36.5: inside a FORMATv2 phase, column-0 non-checkbox lines belong
    // to the PHASE — not whichever task happened to be open on the stack. This
    // captures the FORMATv2 pattern where prose appears BEFORE the first task
    // ("intro paragraph") AND AFTER all tasks ("Random prose that shouldn't be
    // a task, will be swept with the phase."). Blank lines and indented prose
    // (indent > 0) keep the existing stack-attached behavior — indentation
    // signals subordination to the open leaf.
    //
    // v1 plans are unaffected: current_phase stays None throughout parsing
    // (top-level checkboxes promote to Phase at flush time), so the routing
    // falls through to the legacy stack path.
    let column0_prose = match &annotation {
        Annotation::Text { indent, .. } => *indent == 0,
        Annotation::Bullet { indent, .. } => *indent == 0,
        Annotation::CodeBlock { indent, .. } => *indent == 0,
        Annotation::Blank { .. } => false,
    };
    if column0_prose && let Some(phase) = current_phase.as_mut() {
        phase.annotations.push(annotation);
        return;
    }
    if let Some((top, _)) = stack.last_mut() {
        top.annotations.push(annotation);
    } else if let Some(phase) = current_phase.as_mut() {
        // Indented prose (or a blank) before the first task — falls back to
        // phase annotations when the stack is empty.
        phase.annotations.push(annotation);
    }
    // Else: after checkboxes started but no open node and no open phase —
    // shouldn't happen with a balanced tree. Skip silently.
}

/// Append a blank line to the top open node's annotations, coalescing
/// consecutive blanks into a single `Annotation::Blank { count: n }`. Falls
/// back to the open phase's annotations when the stack is empty (so blank
/// lines inside a FORMATv2 phase before its first task round-trip).
fn bump_blank_on_top(stack: &mut [(Node, usize)], current_phase: &mut Option<Phase>) {
    if let Some((top, _)) = stack.last_mut() {
        if let Some(Annotation::Blank { count }) = top.annotations.last_mut() {
            *count += 1;
        } else {
            top.annotations.push(Annotation::Blank { count: 1 });
        }
        return;
    }
    if let Some(phase) = current_phase.as_mut() {
        if let Some(Annotation::Blank { count }) = phase.annotations.last_mut() {
            *count += 1;
        } else {
            phase.annotations.push(Annotation::Blank { count: 1 });
        }
    }
    // Else: orphan blank line outside any phase or open node; drop.
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
        ">" => NodeState::Backlog,
        other => {
            return Err(ParseError::BadCheckboxState {
                line: line_no,
                state: other.to_string(),
            });
        }
    };

    let (id, title) = extract_id_title(rest);
    Ok(CheckboxLine::Checkbox { state, id, title })
}

/// Pull an optional id off the front of the post-checkbox text.
///
/// FORMATv2-only. Accepts:
/// - canonical:  `X.4.a.1 - title`
/// - space-only: `X.4.a.1 title`  (tolerated on read; serialized back as ` - `)
/// - no id:      `do the thing`   (returns `("", "do the thing")`)
///
/// Bold (`**id**`) and em-dash separators are no longer recognized — a line
/// using them parses with an empty id and the raw text as the title. Run
/// `canonicalize` on v0.9 before upgrading if your plan still has them.
fn extract_id_title(input: &str) -> (String, String) {
    let mut input = input;
    id_and_title
        .parse_next(&mut input)
        .expect("id_and_title is total")
}

fn id_and_title(input: &mut &str) -> winnow::ModalResult<(String, String), ContextError> {
    space0.parse_next(input)?;
    let id = opt(bare_id).parse_next(input)?.unwrap_or_default();
    if !id.is_empty() {
        // Consume the id/title separator: a run of spaces and/or hyphens
        // (` `, ` - `, `--`). Em-dash is intentionally NOT consumed — it's a
        // dropped v1 separator, so a stray `—` stays in the title verbatim.
        let _: &str =
            take_while(0.., |c: char| c == '-' || c == ' ' || c == '\t').parse_next(input)?;
    }
    let title = rest.parse_next(input)?.trim_end().to_string();
    Ok((id, title))
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
        let input = "## Phase 1 - First phase\n";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases.len(), 1);
        let phase = &plan.phases[0];
        assert_eq!(phase.id, "1");
        assert_eq!(phase.title, "First phase");
        assert!(!phase.is_done());
        assert!(phase.children.is_empty());
    }

    #[test]
    fn parses_completed_checkbox() {
        let plan = parse("## Phase 1 - Done phase\n- [x] 1.1 - Done task\n").unwrap();
        assert!(plan.phases[0].children[0].is_done());
    }

    #[test]
    fn parses_wont_do_checkbox() {
        let plan = parse("## Phase 1 - Skipped phase\n- [-] 1.1 - Skipped task\n").unwrap();
        let task = &plan.phases[0].children[0];
        assert_eq!(task.state, NodeState::WontDo);
        assert!(!task.is_done());
        assert!(task.is_resolved());
    }

    #[test]
    fn parses_tilde_as_wont_do_alias() {
        let plan = parse("## Phase 1 - Skipped via tilde\n- [~] 1.1 - Skipped task\n").unwrap();
        assert_eq!(plan.phases[0].children[0].state, NodeState::WontDo);
    }

    #[test]
    fn parses_backlog_checkbox() {
        let plan = parse("## Phase 1 - Deferred phase\n- [>] 1.1 - Deferred task\n").unwrap();
        let task = &plan.phases[0].children[0];
        assert_eq!(task.state, NodeState::Backlog);
        assert!(!task.is_done());
        assert!(task.is_resolved());
    }

    #[test]
    fn lifts_trailing_backlog_into_field() {
        let input = "\
## Phase 1 - Phase
- [ ] 1.1 - Task

## Backlog (not yet phased)

- **Deferred A** — added 2026-05-19.
- **Deferred B** — deferred from 1.2 on 2026-05-19.
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases.len(), 1);
        // The backlog bullets are NOT dangling as annotations on the last leaf.
        assert!(plan.phases[0].children[0].annotations.is_empty());
        assert_eq!(
            plan.backlog,
            vec![
                "- **Deferred A** — added 2026-05-19.",
                "- **Deferred B** — deferred from 1.2 on 2026-05-19.",
            ]
        );
    }

    #[test]
    fn trailing_backlog_round_trips_idempotently() {
        let input =
            "## Phase 1 - Phase\n\n## Backlog (not yet phased)\n\n- **X** — added 2026-05-19.\n";
        let plan1 = parse(input).unwrap();
        let out = crate::serializer::serialize(&plan1);
        assert_eq!(
            out, input,
            "serialize should reproduce the trailing section"
        );
        let plan2 = parse(&out).unwrap();
        assert_eq!(plan1, plan2, "parse→serialize→parse must be stable");
    }

    #[test]
    fn does_not_lift_preamble_backlog() {
        // Backlog above the phases stays in the preamble until canonicalize.
        let input = "\
# Title

## Backlog (not yet phased)

- **Early note** — added 2026-05-19.

## Phase 1 - Phase
";
        let plan = parse(input).unwrap();
        assert!(
            plan.backlog.is_empty(),
            "preamble backlog must NOT be lifted"
        );
        assert!(plan.preamble.iter().any(|l| l.contains("Early note")));
    }

    #[test]
    fn does_not_lift_non_backlog_trailing_section() {
        // A `## Sustainment`-style trailing section is left for the tree walk.
        let input = "\
## Phase 1 - Phase

## Sustainment & minor features

- some bullet
";
        let plan = parse(input).unwrap();
        assert!(plan.backlog.is_empty());
    }

    #[test]
    fn parses_nested_three_levels() {
        let input = "\
## Phase 1 - Phase
- [ ] 1.1 - Task
  - [ ] 1.1.1 - Subtask
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
## Phase 1 - Phase
- [ ] 1.1 - Task
    - [ ] 1.1.1 - Subtask
";
        let plan = parse(input).unwrap();
        let phase = &plan.phases[0];
        assert_eq!(phase.children.len(), 1, "1.1 should be child of phase 1");
        assert_eq!(
            phase.children[0].children.len(),
            1,
            "1.1.1 should be child of 1.1"
        );
    }

    #[test]
    fn handles_mixed_indent_across_phases() {
        let input = "\
## Phase 1 - P1
- [ ] 1.1 - T
## Phase 2 - P2
    - [ ] 2.1 - T
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

## Phase 1 - Phase
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.preamble.len(), 4); // header, blank, prose, blank
        assert_eq!(plan.preamble[0], "# Header");
        assert_eq!(plan.phases.len(), 1);
    }

    #[test]
    fn attaches_bullet_annotation_to_node() {
        let input = "\
## Phase 1 - Phase
- note as bullet
- [ ] 1.1 - Task
";
        let plan = parse(input).unwrap();
        let phase = &plan.phases[0];
        assert_eq!(phase.annotations.len(), 1);
        match &phase.annotations[0] {
            Annotation::Bullet { text, indent } => {
                assert_eq!(text, "note as bullet");
                assert_eq!(*indent, 0);
            }
            other => panic!("expected Bullet, got {other:?}"),
        }
        assert_eq!(phase.children.len(), 1);
    }

    #[test]
    fn attaches_text_annotation_to_node() {
        let input = "\
## Phase 1 - Phase
Some context for the phase.
";
        let plan = parse(input).unwrap();
        let phase = &plan.phases[0];
        assert_eq!(phase.annotations.len(), 1);
        match &phase.annotations[0] {
            Annotation::Text { text, indent } => {
                assert_eq!(text, "Some context for the phase.");
                assert_eq!(*indent, 0);
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn attaches_code_block_annotation() {
        let input = "\
## Phase 1 - Phase
```rust
fn foo() {}
```
";
        let plan = parse(input).unwrap();
        let phase = &plan.phases[0];
        assert_eq!(phase.annotations.len(), 1);
        match &phase.annotations[0] {
            Annotation::CodeBlock {
                lang,
                content,
                indent,
            } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert!(content.contains("fn foo()"));
                assert_eq!(*indent, 0);
            }
            other => panic!("expected CodeBlock, got {other:?}"),
        }
    }

    #[test]
    fn tolerates_checkbox_without_id() {
        let plan = parse("## Phase 1 - Phase\n- [ ] no id here\n").unwrap();
        let task = &plan.phases[0].children[0];
        assert_eq!(task.id, "");
        assert_eq!(task.title, "no id here");
    }

    #[test]
    fn parses_alphanumeric_id_without_bold() {
        // FORMATv2: alphanumeric task ids (`X.4.a.1`) parse plain — no bold
        // wrapping — under a phase header.
        let plan = parse("## Phase X - Title\n- [ ] X.4.a.1 - title here\n").unwrap();
        let task = &plan.phases[0].children[0];
        assert_eq!(task.id, "X.4.a.1");
        assert_eq!(task.title, "title here");
    }

    #[test]
    fn does_not_treat_first_title_word_as_id() {
        // "Make" has no dot and isn't all-digits, so it must NOT be grabbed as an id.
        let plan =
            parse("## Phase 1 - Phase\n- [ ] Make the core domain model the source\n").unwrap();
        let task = &plan.phases[0].children[0];
        assert_eq!(task.id, "");
        assert_eq!(task.title, "Make the core domain model the source");
    }

    #[test]
    fn section_header_inside_tree_attaches_as_text_annotation() {
        let input = "\
## Phase 1 - Phase
- [ ] 1.1 - Task
  ## A markdown heading
";
        let plan = parse(input).unwrap();
        let phase = &plan.phases[0];
        let task = &phase.children[0];
        assert_eq!(task.annotations.len(), 1);
        match &task.annotations[0] {
            Annotation::Text { text, .. } => assert_eq!(text, "## A markdown heading"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert_eq!(
            phase.children.len(),
            1,
            "1.1 should still be a child of phase 1"
        );
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
        let plan = parse(&input).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
        assert!(
            !plan.phases.is_empty(),
            "quicksight PLAN.md should have phases"
        );
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
        // Short numeric ids (`1`, no dot) are valid both for the phase header
        // and for a bare task id under it.
        let input = "\
## Phase 1 - Phase
- [ ] 1 - Task
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases[0].id, "1");
        assert_eq!(plan.phases[0].children[0].id, "1");
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
        assert_eq!(p1.id, "1");
        assert!(!p1.is_done());
        assert_eq!(p1.children.len(), 2);

        let t11 = &p1.children[0];
        assert_eq!(t11.id, "1.1");
        assert_eq!(t11.children.len(), 2);
        assert!(t11.children[0].is_done(), "1.1.1 should be checked");
        assert!(!t11.children[1].is_done(), "1.1.2 should be unchecked");

        let t12 = &p1.children[1];
        assert_eq!(t12.id, "1.2");
        assert_eq!(
            t12.annotations.len(),
            3,
            "1.2 has text + bullet + trailing-blank annotations"
        );

        let p2 = &plan.phases[1];
        assert_eq!(p2.id, "2");
        assert_eq!(p2.children.len(), 1);
        assert!(p2.children[0].is_done());
    }

    #[test]
    fn parses_this_repos_plan_md() {
        // e2e smoke test: the live PLAN.md in this repo must always PARSE.
        // No assertion on content — between a phase exit and the next plan
        // entry, PLAN.md is legitimately empty of phases, and we don't want
        // archive sweeps to break the release pipeline.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("PLAN.md");
        let input = std::fs::read_to_string(&path).expect("PLAN.md exists");
        parse(&input).expect("PLAN.md must parse cleanly");
    }

    #[test]
    fn ignores_blank_lines_inside_tree() {
        let input = "\
## Phase 1 - Phase

- [ ] 1.1 - Task

- [ ] 1.2 - Other
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases[0].children.len(), 2);
    }

    // -----------------------------------------------------------------
    // Phase 36.3: FORMATv2 phase-header parsing
    // -----------------------------------------------------------------

    #[test]
    fn v2_header_opens_a_phase_with_tasks_as_children() {
        let input = "\
## Phase AI - Studio dogfood

- [ ] AI.0 Lock decisions
- [x] AI.1 Audit
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases.len(), 1);
        let p = &plan.phases[0];
        assert_eq!(p.id, "AI");
        assert_eq!(p.title, "Studio dogfood");
        assert!(p.depends_on.is_empty());
        assert!(p.prefer_after.is_empty());
        assert_eq!(p.children.len(), 2);
        assert_eq!(p.children[0].id, "AI.0");
        assert_eq!(p.children[1].id, "AI.1");
        assert!(p.children[1].is_done());
    }

    #[test]
    fn v2_header_parses_depends_on_marker() {
        let input = "\
## Phase AS - Spine *(depends on: AR)*

- [ ] AS.0 plan
";
        let plan = parse(input).unwrap();
        let p = &plan.phases[0];
        assert_eq!(p.id, "AS");
        assert_eq!(p.title, "Spine");
        assert_eq!(p.depends_on, vec!["AR"]);
        assert!(p.prefer_after.is_empty());
    }

    #[test]
    fn v2_header_parses_prefer_after_marker() {
        let input = "\
## Phase AM - Tailwind *(prefer after: AI)*

- [ ] AM.0 plan
";
        let plan = parse(input).unwrap();
        let p = &plan.phases[0];
        assert_eq!(p.depends_on, Vec::<String>::new());
        assert_eq!(p.prefer_after, vec!["AI"]);
    }

    #[test]
    fn v2_header_parses_both_markers_either_order() {
        let input = "\
## Phase AS - Spine *(depends on: AR, AQ)* *(prefer after: AB)*
";
        let plan = parse(input).unwrap();
        let p = &plan.phases[0];
        assert_eq!(p.depends_on, vec!["AR", "AQ"]);
        assert_eq!(p.prefer_after, vec!["AB"]);

        // Reversed order also works.
        let input2 = "## Phase AS - Spine *(prefer after: AB)* *(depends on: AR)*\n";
        let plan2 = parse(input2).unwrap();
        let p2 = &plan2.phases[0];
        assert_eq!(p2.depends_on, vec!["AR"]);
        assert_eq!(p2.prefer_after, vec!["AB"]);
    }

    #[test]
    fn v2_header_with_no_markers_and_no_separator_is_id_only() {
        // The bare `## Phase AI` form (no title) parses as id-only, title="".
        // Matches the user's current quicksight PLAN.md mid-pivot state.
        let input = "## Phase AI\n\n- [ ] AI.0 task\n";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases[0].id, "AI");
        assert_eq!(plan.phases[0].title, "");
        assert_eq!(plan.phases[0].children.len(), 1);
    }

    #[test]
    fn v2_header_does_not_match_indented_or_h3_or_h1() {
        // `### Phase X` (h3) → not a v2 phase header (h2 only). With no real
        // `## Phase` header, the h3 line lands in the preamble and there are no
        // phases.
        let plan = parse("### Phase AI - Title\n").unwrap();
        assert!(plan.phases.is_empty());

        // Indented `  ## Phase X` is task-internal markdown, not a header.
        let plan = parse("## Phase 1 - outer\n- [ ] 1.1 - inner\n  ## Phase X\n").unwrap();
        assert_eq!(plan.phases.len(), 1);
        assert_eq!(plan.phases[0].id, "1");
    }

    #[test]
    fn v2_header_does_not_match_backlog_heading() {
        // `## Backlog (not yet phased)` is a backlog section, NOT a phase
        // header. The trailing-backlog peel handles it; if mid-document, the
        // existing extract_backlog_from_* paths sweep it.
        let plan = parse("## Phase 1 - Phase\n\n## Backlog (not yet phased)\n").unwrap();
        // Single phase, no v2 phase added for `## Backlog`.
        assert_eq!(plan.phases.len(), 1);
        assert_eq!(plan.phases[0].id, "1");
    }

    #[test]
    fn two_v2_headers_in_a_row_each_become_a_phase() {
        let input = "\
## Phase AI - First
- [ ] AI.0 a
## Phase AS - Second *(depends on: AI)*
- [ ] AS.0 b
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases.len(), 2);
        assert_eq!(plan.phases[0].id, "AI");
        assert_eq!(plan.phases[0].children.len(), 1);
        assert_eq!(plan.phases[1].id, "AS");
        assert_eq!(plan.phases[1].depends_on, vec!["AI"]);
    }

    // -----------------------------------------------------------------
    // Phase 36.4: FORMATv2 backlog h1 + nested descoped subtrees
    // -----------------------------------------------------------------

    #[test]
    fn h1_backlog_heading_is_recognized_on_read() {
        let input = "\
## Phase 1 - Phase

# Backlog (not yet phased)

- **Item A** — added 2026-05-19.
- **Item B** — deferred from 1.2 on 2026-05-19.
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases.len(), 1);
        // h1 backlog peeled into plan.backlog just like h2 did.
        assert_eq!(
            plan.backlog,
            vec![
                "- **Item A** — added 2026-05-19.",
                "- **Item B** — deferred from 1.2 on 2026-05-19.",
            ]
        );
    }

    #[test]
    fn h1_backlog_preserves_nested_descoped_subtrees() {
        // FORMATv2 sweep-on-archive puts descoped phase contents into Backlog
        // as nested plain bullets with optional continuation prose. All lines
        // must round-trip verbatim.
        let input = "\
## Phase 1 - Phase

# Backlog (not yet phased)

- Plain backlog item
- X.1 - Descoped item from a swept phase
  - X.1.1 - Subtask swept with parent to backlog
    Prose for the task
";
        let plan = parse(input).unwrap();
        assert_eq!(
            plan.backlog,
            vec![
                "- Plain backlog item",
                "- X.1 - Descoped item from a swept phase",
                "  - X.1.1 - Subtask swept with parent to backlog",
                "    Prose for the task",
            ],
            "nested descoped subtree must round-trip line-for-line"
        );
    }

    #[test]
    fn h1_backlog_in_preamble_consolidates_to_field() {
        // Mid-document `# Backlog` (above the phases) should consolidate into
        // plan.backlog the same way the legacy h2 form did.
        let input = "\
# Title

# Backlog (not yet phased)

- **Early note** — added 2026-05-19.
- X.1 - Descoped from elsewhere
  - X.1.1 - subtask
    notes

## Phase 1 - Phase
";
        let mut plan = parse(input).unwrap();
        // Preamble form isn't auto-lifted (only trailing blocks are).
        assert!(plan.backlog.is_empty());
        let swept = plan.consolidate_backlog();
        assert_eq!(swept, 4);
        assert_eq!(
            plan.backlog,
            vec![
                "- **Early note** — added 2026-05-19.",
                "- X.1 - Descoped from elsewhere",
                "  - X.1.1 - subtask",
                "    notes",
            ]
        );
        // Heading removed from preamble.
        assert!(
            !plan
                .preamble
                .iter()
                .any(|l| crate::ast::is_backlog_heading(l))
        );
    }

    #[test]
    fn h2_legacy_backlog_still_recognized() {
        // Forward-compat: old bridge versions wrote `## Backlog`; the parser
        // must keep accepting it on read so installed projects don't break
        // when the bridge upgrades.
        let input = "\
## Phase 1 - Phase

## Backlog (not yet phased)

- **Legacy** — added 2026-05-19.
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.backlog, vec!["- **Legacy** — added 2026-05-19."]);
    }

    #[test]
    fn v2_phase_prose_before_first_task_lands_as_annotation() {
        let input = "\
## Phase AI - Studio dogfood

Some intro prose here.

- [ ] AI.0 task
";
        let plan = parse(input).unwrap();
        let p = &plan.phases[0];
        assert_eq!(p.id, "AI");
        assert!(
            p.annotations.iter().any(
                |a| matches!(a, Annotation::Text { text, .. } if text.contains("intro prose"))
            ),
            "phase-level prose should be captured in annotations: {:?}",
            p.annotations
        );
    }

    // -----------------------------------------------------------------
    // Phase 36.5: phase-level prose bucketing (column-0 in v2 phases
    // attaches to the phase, not the open task stack)
    // -----------------------------------------------------------------

    #[test]
    fn v2_phase_column0_prose_after_tasks_attaches_to_phase_not_last_task() {
        // The FORMATv2 contract: column-0 prose AFTER all tasks in a v2 phase
        // belongs to the phase (it'll be swept with the phase at archive time),
        // not to whichever task happened to be the last open node on the stack.
        let input = "\
## Phase AI - Studio dogfood

- [ ] AI.0 first
- [ ] AI.1 second

Random prose that shouldn't be a task, will be swept with the phase.
";
        let plan = parse(input).unwrap();
        let p = &plan.phases[0];
        assert_eq!(p.children.len(), 2);
        // The last task (AI.1) must NOT have the trailing prose attached.
        let ai_1 = &p.children[1];
        assert!(
            !ai_1.annotations.iter().any(
                |a| matches!(a, Annotation::Text { text, .. } if text.contains("Random prose"))
            ),
            "trailing column-0 prose must NOT attach to AI.1: {:?}",
            ai_1.annotations
        );
        // The phase itself must own the trailing prose.
        assert!(
            p.annotations.iter().any(
                |a| matches!(a, Annotation::Text { text, .. } if text.contains("Random prose"))
            ),
            "trailing column-0 prose must attach to phase: {:?}",
            p.annotations
        );
    }

    #[test]
    fn v2_phase_indented_prose_under_task_still_attaches_to_task() {
        // Subordination signal: indented prose under a leaf belongs to the
        // leaf, not the phase. (Same v1 behavior — only column-0 prose was
        // re-routed.)
        let input = "\
## Phase AI - Studio

- [ ] AI.0 first task
  This is task-level prose, indented under AI.0.
- [ ] AI.1 second
";
        let plan = parse(input).unwrap();
        let p = &plan.phases[0];
        let ai_0 = &p.children[0];
        assert!(
            ai_0.annotations.iter().any(
                |a| matches!(a, Annotation::Text { text, .. } if text.contains("task-level prose"))
            ),
            "indented prose stays with the task it sits under: {:?}",
            ai_0.annotations
        );
        // Phase annotations don't accidentally receive task-level prose.
        assert!(
            !p.annotations.iter().any(
                |a| matches!(a, Annotation::Text { text, .. } if text.contains("task-level prose"))
            ),
            "indented prose must NOT bubble up to phase: {:?}",
            p.annotations
        );
    }

    #[test]
    fn v2_phase_multiple_prose_blocks_around_tasks_all_bucket_on_phase() {
        // Intro prose, prose between tasks, trailing prose — all column-0,
        // all phase-level.
        let input = "\
## Phase AI - Title

Intro paragraph.

- [ ] AI.0 first

Prose between tasks.

- [ ] AI.1 second

Closing prose.
";
        let plan = parse(input).unwrap();
        let p = &plan.phases[0];
        let texts: Vec<&str> = p
            .annotations
            .iter()
            .filter_map(|a| {
                if let Annotation::Text { text, .. } = a {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(
            texts.iter().any(|t| t.contains("Intro paragraph")),
            "intro at phase: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Prose between tasks")),
            "between-tasks prose at phase: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Closing prose")),
            "closing prose at phase: {texts:?}"
        );
    }

    // -----------------------------------------------------------------
    // Phase 36.6: end-to-end fixture coverage. `tests/fixtures/v2_mixed.md`
    // exercises every FORMATv2 feature in one document (v1 anchor + v2
    // header phases, depends_on, prefer_after, phase-level prose, nested
    // tasks, all four states, h1 backlog with nested descoped subtrees).
    // -----------------------------------------------------------------

    #[test]
    fn parses_v2_mixed_fixture_with_all_features() {
        let input = include_str!("../tests/fixtures/v2_mixed.md");
        let plan = parse(input).unwrap();

        // 5 phases total: 1, then AI / AQ / AM / AS.
        let ids: Vec<&str> = plan.phases.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["1", "AI", "AQ", "AM", "AS"]);

        // Phase 1 carries tasks with done / won't-do / backlog states.
        let first = &plan.phases[0];
        assert_eq!(first.children.len(), 3);
        assert_eq!(first.children[0].state, NodeState::Done);
        assert_eq!(first.children[1].state, NodeState::WontDo);
        assert_eq!(first.children[2].state, NodeState::Backlog);
        assert!(
            first.depends_on.is_empty() && first.prefer_after.is_empty(),
            "this phase has no FORMATv2 dependency metadata"
        );

        // AI: header phase, intro prose, between-task prose, trailing prose.
        let ai = &plan.phases[1];
        assert_eq!(ai.title, "Studio dogfood");
        assert_eq!(ai.children.len(), 4);
        let phase_text_blobs: Vec<&str> = ai
            .annotations
            .iter()
            .filter_map(|a| match a {
                Annotation::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            phase_text_blobs
                .iter()
                .any(|t| t.contains("Intro paragraph")),
            "intro prose at phase: {phase_text_blobs:?}"
        );
        assert!(
            phase_text_blobs
                .iter()
                .any(|t| t.contains("Prose between tasks")),
            "between-task prose at phase: {phase_text_blobs:?}"
        );
        assert!(
            phase_text_blobs
                .iter()
                .any(|t| t.contains("Trailing prose for Phase AI")),
            "trailing prose at phase: {phase_text_blobs:?}"
        );

        // AI.1.1 carries the indented (task-level) prose, NOT the phase.
        let ai_1 = &ai.children[1];
        let ai_1_1 = &ai_1.children[1];
        assert_eq!(ai_1_1.id, "AI.1.1");
        assert!(
            ai_1_1.annotations.iter().any(
                |a| matches!(a, Annotation::Text { text, .. } if text.contains("Indented prose"))
            ),
            "indented task-level prose stays with the task"
        );

        // AQ has depends_on=[AP], no prefer_after.
        let aq = &plan.phases[2];
        assert_eq!(aq.depends_on, vec!["AP"]);
        assert!(aq.prefer_after.is_empty());

        // AM has prefer_after=[AI], no depends_on.
        let am = &plan.phases[3];
        assert!(am.depends_on.is_empty());
        assert_eq!(am.prefer_after, vec!["AI"]);

        // AS has both markers.
        let r#as = &plan.phases[4];
        assert_eq!(r#as.depends_on, vec!["AR", "AQ"]);
        assert_eq!(r#as.prefer_after, vec!["AM"]);

        // Backlog: h1 form, both flat notes and nested descoped subtrees.
        assert_eq!(plan.backlog.len(), 6);
        assert!(plan.backlog.iter().any(|l| l.contains("**Plain note**")));
        assert!(
            plan.backlog
                .iter()
                .any(|l| l.contains("**Deferred from a phase**"))
        );
        assert!(
            plan.backlog
                .iter()
                .any(|l| l == "- X.1 - Descoped item from an archived phase")
        );
        assert!(
            plan.backlog
                .iter()
                .any(|l| l == "  - X.1.1 - Subtask carried over with structure intact")
        );
        assert!(
            plan.backlog
                .iter()
                .any(|l| l == "    Prose continuation under the descoped subtask.")
        );
        assert!(
            plan.backlog
                .iter()
                .any(|l| l == "- Y.7 - Another descoped subtree")
        );
    }

    #[test]
    fn v2_mixed_fixture_round_trips_through_serialize() {
        // FORMATv2: header phases serialize via `## Phase X - Title` with tasks
        // dedented to column 0. Full round-trip is AST-stable.
        let input = include_str!("../tests/fixtures/v2_mixed.md");
        let plan1 = parse(input).unwrap();
        let out = crate::serializer::serialize(&plan1);
        let plan2 = parse(&out).unwrap();
        assert_eq!(
            plan1, plan2,
            "AST must be stable across parse → serialize → parse"
        );
    }

    #[test]
    fn column0_checkbox_without_phase_header_is_orphan_error() {
        // FORMATv2: a column-0 checkbox with no `## Phase` header above it is
        // rejected — the legacy `- [ ] N.0` anchor form is gone.
        let err = parse("- [ ] 1.0 Phase\n  - [ ] 1.1 task\n").unwrap_err();
        assert!(matches!(err, ParseError::OrphanCheckbox { .. }));
    }
}

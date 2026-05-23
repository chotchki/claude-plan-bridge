use crate::ast::{Annotation, IdStyle, Node, NodeState, Phase, Plan, Separator};
use thiserror::Error;
use winnow::Parser;
use winnow::ascii::space0;
use winnow::combinator::{alt, delimited, opt};
use winnow::error::ContextError;
use winnow::token::{rest, take_until, take_while};

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
    let (body, trailing_backlog) = peel_trailing_backlog(input);
    plan.backlog = trailing_backlog;

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
                push_into_parent(&mut plan, &mut stack, &mut current_phase, node);
            }
            if let Some(p) = current_phase.take() {
                plan.phases.push(p);
            }
            current_phase = Some(Phase {
                id: header.id,
                title: header.title,
                state: NodeState::Pending,
                id_style: IdStyle::Plain,
                // FORMATv2 canonical: hyphen-space separator. Phase 37.1's
                // serializer reads this to emit ` - ` on the header line.
                separator: Separator::Hyphen,
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
            CheckboxLine::Checkbox {
                state,
                id,
                title,
                id_style,
                separator,
            } => {
                saw_checkbox = true;
                let node = Node {
                    id,
                    title,
                    state,
                    id_style,
                    separator,
                    children: Vec::new(),
                    annotations: Vec::new(),
                };
                // Pop deeper-or-equal nodes off the stack (they're complete).
                while let Some((_, top_indent)) = stack.last() {
                    if *top_indent >= indent {
                        let (done, _) = stack.pop().unwrap();
                        push_into_parent(&mut plan, &mut stack, &mut current_phase, done);
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
        push_into_parent(&mut plan, &mut stack, &mut current_phase, node);
    }
    if let Some(p) = current_phase.take() {
        plan.phases.push(p);
    }

    plan.preamble = preamble;
    Ok(plan)
}

fn push_into_parent(
    plan: &mut Plan,
    stack: &mut [(Node, usize)],
    current_phase: &mut Option<Phase>,
    node: Node,
) {
    if let Some((parent, _)) = stack.last_mut() {
        parent.children.push(node);
    } else if let Some(phase) = current_phase.as_mut() {
        // Inside a FORMATv2 phase: top-level checkboxes become its tasks.
        phase.children.push(node);
    } else {
        // No header context — legacy v1 `- [ ] N.0` anchor. Promote to Phase
        // via from_node so the Node-shaped state/style/separator come along.
        plan.phases.push(Phase::from_node(node));
    }
}

/// Parsed FORMATv2 phase header (`## Phase <ID> - <Title>` with optional
/// `*(depends on: ...)*` and/or `*(prefer after: ...)*` markers).
struct PhaseHeader {
    id: String,
    title: String,
    depends_on: Vec<String>,
    prefer_after: Vec<String>,
}

/// Match a FORMATv2 phase header line. Returns None for any line that isn't
/// the exact shape `## Phase <alphanumeric-ID><sep><title>` at column 0,
/// including `## Backlog`, `### Phase`, indented headers, headers without
/// the `Phase ` keyword, and bare `## Notes` style sections.
///
/// Separator tolerance on read (canonical write is ` - `): ` - ` hyphen,
/// ` — ` em-dash, or a plain space following the ID — same liberality as
/// checkbox lines.
fn parse_v2_phase_header(line: &str) -> Option<PhaseHeader> {
    // Column-0 only. An indented `## Phase X` is markdown content inside a
    // task, not a phase boundary.
    if line.starts_with(' ') || line.starts_with('\t') {
        return None;
    }
    let after_hashes = line.strip_prefix("## ")?;
    let after_phase_keyword = after_hashes.strip_prefix("Phase ")?;

    // Extract the ID (alphanumeric, may include `.` for legacy/edge cases).
    let id_end = after_phase_keyword
        .find(|c: char| c.is_whitespace() || c == '—' || c == '-' || c == '*')
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

    // Strip the separator (` - ` / ` — ` / bare whitespace). After the rest
    // may still start with `-` or `—` from the separator itself.
    let after_sep = rest
        .trim_start_matches(|c: char| c == '—' || c == '-')
        .trim_start();

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
fn peel_trailing_backlog(input: &str) -> (Vec<&str>, Vec<String>) {
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
            // Found the heading bounding a clean trailing block.
            let body_lines: Vec<String> =
                lines[i..].iter().map(|s| s.to_string()).collect::<Vec<_>>();
            // Trim surrounding blank lines from the captured bullets.
            let backlog = trim_blank_edges(body_lines);
            if backlog.is_empty() {
                // A bare heading with no bullets — not worth lifting.
                return (lines, Vec::new());
            }
            let mut kept: Vec<&str> = lines[..i - 1].to_vec();
            // Drop trailing blanks so re-serialize doesn't accumulate them
            // (the serializer inserts exactly one blank before the section).
            while kept.last().is_some_and(|l| l.trim().is_empty()) {
                kept.pop();
            }
            return (kept, backlog);
        }
        if !is_body_shaped {
            break;
        }
        i -= 1;
    }
    (lines, Vec::new())
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
    if let Some((top, _)) = stack.last_mut() {
        top.annotations.push(annotation);
    } else if let Some(phase) = current_phase.as_mut() {
        // Phase 36.3: lines between a `## Phase X - Title` header and the
        // first task land as phase-level annotations. 36.5 will split this
        // into a dedicated `prose` bucket; for now they share annotations.
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
        id_style: crate::ast::IdStyle,
        separator: crate::ast::Separator,
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

    let (id, title, id_style, separator) = extract_id_title_style(rest);
    Ok(CheckboxLine::Checkbox {
        state,
        id,
        title,
        id_style,
        separator,
    })
}

/// Pull an optional id off the front of the post-checkbox text.
///
/// Accepts:
/// - bold-wrapped ids:  `**X.4.a.1** — title`
/// - bare ids:          `X.4.a.1 title` or `1.0 title`
/// - no id:             `do the thing` (returns `("", "do the thing", Plain)`)
///
/// Returns the captured `IdStyle` so the serializer can round-trip the source
/// format. The joined-bold strategy (where the bold span wraps id + title)
/// can't be fully preserved on round-trip; it flattens to `Plain` for now —
/// the id and title are still parsed correctly, only the bold-wrap is lost.
fn extract_id_title_style(
    input: &str,
) -> (String, String, crate::ast::IdStyle, crate::ast::Separator) {
    let mut input = input;
    id_and_title
        .parse_next(&mut input)
        .expect("id_and_title is total")
}

fn id_and_title(
    input: &mut &str,
) -> winnow::ModalResult<(String, String, crate::ast::IdStyle, crate::ast::Separator), ContextError>
{
    use crate::ast::{IdStyle, Separator};
    space0.parse_next(input)?;

    // Strategy A — joined-bold `**id — title.**` (the bold span contains BOTH
    // the id AND part of the title). Surfaces during the quicksight shakeout:
    // `- [x] **AA.A.1 — Audit existing dropdowns.** Walked L1...`.
    let snapshot = *input;
    if let Ok((id, title_inside)) = joined_bold_id_title.parse_next(input) {
        let trailing_raw: &str = rest.parse_next(input)?;
        let trailing = trailing_raw.trim();
        let title = if trailing.is_empty() {
            title_inside
        } else if title_inside.is_empty() {
            trailing.to_string()
        } else {
            format!("{title_inside} {trailing}")
        };
        // Joined-bold flattens to Plain on round-trip (the bold span covers
        // both id + title; preserving it would require a different style enum
        // variant). Acceptable for now — this shape is rare.
        return Ok((id, title, IdStyle::Plain, Separator::Space));
    }
    *input = snapshot;

    // Strategy B — existing path: simple `**id**` or `id`, then separator,
    // then title. Capture which style + separator matched so the serializer
    // can round-trip.
    let id_match = opt(alt((
        bold_id.map(|id| (id, IdStyle::Bold)),
        bare_id.map(|id| (id, IdStyle::Plain)),
    )))
    .parse_next(input)?;
    let (id, id_style) = id_match.unwrap_or_else(|| (String::new(), IdStyle::Plain));
    let separator = if id.is_empty() {
        Separator::Space
    } else {
        capture_separator.parse_next(input)?
    };
    let title = rest.parse_next(input)?.trim_end().to_string();
    Ok((id, title, id_style, separator))
}

fn bold_id(input: &mut &str) -> winnow::ModalResult<String, ContextError> {
    delimited("**", id_chars, "**").parse_next(input)
}

/// Matches `**<id> <separator> <title-content>**` where the bold span contains
/// both the id and the title text (joined). Caller gets `(id, title_inside)`;
/// trailing prose after the closing `**` is captured separately.
fn joined_bold_id_title(input: &mut &str) -> winnow::ModalResult<(String, String), ContextError> {
    "**".parse_next(input)?;
    let id = id_chars.parse_next(input)?;
    // Require at least one separator char (em-dash, hyphen, or whitespace)
    // after the id, else this is just `**id**` — let Strategy B handle it.
    let _: &str =
        take_while(1.., |c: char| c == '—' || c == '-' || c.is_whitespace()).parse_next(input)?;
    let title_inside: &str = take_until(0.., "**").parse_next(input)?;
    "**".parse_next(input)?;
    Ok((id, title_inside.trim().to_string()))
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

fn capture_separator(input: &mut &str) -> winnow::ModalResult<crate::ast::Separator, ContextError> {
    use crate::ast::Separator;
    let chunk: &str =
        take_while(0.., |c: char| c == '—' || c == '-' || c.is_whitespace()).parse_next(input)?;
    let sep = if chunk.contains('—') {
        Separator::EmDash
    } else if chunk.contains('-') {
        Separator::Hyphen
    } else {
        Separator::Space
    };
    Ok(sep)
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
    fn parses_backlog_checkbox() {
        let plan = parse("- [>] 1.0 Deferred phase\n").unwrap();
        assert_eq!(plan.phases[0].state, NodeState::Backlog);
        assert!(!plan.phases[0].is_done());
        assert!(plan.phases[0].is_resolved());
    }

    #[test]
    fn lifts_trailing_backlog_into_field() {
        let input = "\
- [ ] 1.0 Phase
  - [ ] 1.1 Task

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
            "- [ ] 1.0 Phase\n\n## Backlog (not yet phased)\n\n- **X** — added 2026-05-19.\n";
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

- [ ] 1.0 Phase
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
- [ ] 1.0 Phase

## Sustainment & minor features

- some bullet
";
        let plan = parse(input).unwrap();
        assert!(plan.backlog.is_empty());
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
        assert_eq!(
            phase.children[0].children.len(),
            1,
            "1.1.1 should be child of 1.1"
        );
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
            Annotation::CodeBlock {
                lang,
                content,
                indent,
            } => {
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
    fn parses_joined_bold_id_title_with_trailing_prose() {
        // Phase 16.2 regression — quicksight shakeout. Bold span contains
        // BOTH id and title (period included), followed by more prose.
        let plan =
            parse("- [x] **AA.A.1 — Audit existing dropdowns.** Walked L1 / L2 / forms\n").unwrap();
        assert_eq!(plan.phases[0].id, "AA.A.1");
        assert_eq!(
            plan.phases[0].title,
            "Audit existing dropdowns. Walked L1 / L2 / forms"
        );
        assert!(plan.phases[0].is_done());
    }

    #[test]
    fn parses_joined_bold_no_trailing_prose() {
        let plan = parse("- [ ] **1.0 — Just the bold contents**\n").unwrap();
        assert_eq!(plan.phases[0].id, "1.0");
        assert_eq!(plan.phases[0].title, "Just the bold contents");
    }

    #[test]
    fn parses_joined_bold_with_space_separator() {
        // Em-dash isn't required — any separator works.
        let plan = parse("- [ ] **AA.A.1 Audit dropdowns.** trailing\n").unwrap();
        assert_eq!(plan.phases[0].id, "AA.A.1");
        assert_eq!(plan.phases[0].title, "Audit dropdowns. trailing");
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
        assert_eq!(
            plan.phases[0].title,
            "Make the core domain model the source"
        );
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
        assert_eq!(
            phase.children.len(),
            1,
            "1.1 should still be a child of 1.0"
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
        assert_eq!(
            t12.annotations.len(),
            2,
            "1.2 has text + bullet annotations"
        );

        let p2 = &plan.phases[1];
        assert_eq!(p2.id, "2.0");
        assert!(p2.is_done());
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
- [ ] 1.0 Phase

  - [ ] 1.1 Task

  - [ ] 1.2 Other
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
    fn v2_header_with_em_dash_separator_still_parses() {
        // Tolerance: read-side accepts em-dash even though canonical write is
        // hyphen-space.
        let input = "## Phase AI — Studio dogfood\n";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases[0].id, "AI");
        assert_eq!(plan.phases[0].title, "Studio dogfood");
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
        // `### Phase X` (h3) → not a v2 phase header (h2 only).
        let plan = parse("### Phase AI - Title\n- [ ] AI.0 task\n").unwrap();
        // Falls back to legacy: `### Phase` becomes a text annotation; the
        // top-level checkbox promotes to a v1 Phase.
        assert!(plan.phases[0].id != "AI" || plan.phases.len() != 1);

        // Indented `  ## Phase X` is task-internal markdown, not a header.
        let plan = parse("- [ ] 1.0 outer\n  ## Phase X\n  - [ ] 1.1 inner\n").unwrap();
        assert_eq!(plan.phases[0].id, "1.0");
    }

    #[test]
    fn v2_header_does_not_match_backlog_heading() {
        // `## Backlog (not yet phased)` is a backlog section, NOT a phase
        // header. The trailing-backlog peel handles it; if mid-document, the
        // existing extract_backlog_from_* paths sweep it.
        let plan = parse("- [ ] 1.0 Phase\n\n## Backlog (not yet phased)\n").unwrap();
        // Single phase, no v2 phase added for `## Backlog`.
        assert_eq!(plan.phases.len(), 1);
        assert_eq!(plan.phases[0].id, "1.0");
    }

    #[test]
    fn v2_header_phase_followed_by_v1_anchor_keeps_anchor_as_task() {
        // Mixed plan: a `## Phase AI` header followed by a `- [ ] 36.0 ...`
        // checkbox lands the checkbox as a task of AI (since we're inside the
        // v2 phase). User-visible weirdness, but the canonicalize path can
        // sort it out — for now, preserve everything.
        let input = "## Phase AI - Header phase\n\n- [ ] 36.0 Legacy anchor\n";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases.len(), 1);
        assert_eq!(plan.phases[0].id, "AI");
        assert_eq!(plan.phases[0].children.len(), 1);
        assert_eq!(plan.phases[0].children[0].id, "36.0");
    }

    #[test]
    fn v1_anchors_before_first_v2_header_still_become_phases() {
        // Legacy phases lead, FORMATv2 header follows. Both coexist.
        let input = "\
- [ ] 1.0 Legacy phase

## Phase AI - New phase

- [ ] AI.0 New work
";
        let plan = parse(input).unwrap();
        assert_eq!(plan.phases.len(), 2);
        assert_eq!(plan.phases[0].id, "1.0");
        assert_eq!(plan.phases[1].id, "AI");
        assert_eq!(plan.phases[1].children.len(), 1);
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
        // Prose lives in annotations until 36.5's dedicated prose bucket.
        assert!(
            p.annotations
                .iter()
                .any(|a| matches!(a, Annotation::Text { text, .. } if text.contains("intro prose"))),
            "phase-level prose should be captured in annotations: {:?}",
            p.annotations
        );
    }
}

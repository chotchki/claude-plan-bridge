use crate::ast::{Annotation, Node, NodeState, Phase, Plan};

/// Render a `Plan` back to markdown.
///
/// Normalizes to 2-space indent per tree level. Preamble lines are preserved
/// verbatim. Code-block content is preserved verbatim (its inner indent is part
/// of the content); only the surrounding ``` fences are re-emitted at the
/// normalized indent. Blank lines inside the tree are not reconstructed (parser
/// discards them), so round-trip is AST-stable, not byte-stable.
pub fn serialize(plan: &Plan) -> String {
    let mut out = String::new();
    for line in &plan.preamble {
        out.push_str(line);
        out.push('\n');
    }
    for phase in &plan.phases {
        // Phase 29.6: blanks between top-level phases come from
        // captured `Annotation::Blank` on the prior phase's last open child,
        // not from a serializer-side auto-insertion. Removes the asymmetry
        // that caused roundtrip drift (serialize emits a blank that parse
        // then captures as a new Blank annotation, growing the AST each cycle).
        write_phase(&mut out, phase);
    }
    // Phase 35: the canonical Backlog section renders last, below every phase.
    // One blank line separates it from preceding content. The parser auto-lifts
    // this trailing block back into `plan.backlog` on the next parse, so the
    // round-trip is stable and a later phase-append can't slip ahead of it.
    //
    // Phase 37.2: heading level is per-plan (`plan.backlog_h1`). FORMATv2
    // canonical is `# Backlog (not yet phased)` (h1); legacy/parsed h2 form
    // round-trips as h2 unless `canonicalize` flips it.
    if !plan.backlog.is_empty() {
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        if plan.backlog_h1 {
            out.push_str("# Backlog (not yet phased)\n\n");
        } else {
            out.push_str("## Backlog (not yet phased)\n\n");
        }
        for line in &plan.backlog {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// FORMATv2 header form (the only form): `## Phase <id> - <title>` with
/// optional `*(depends on: ...)*` / `*(prefer after: ...)*` markers, phase
/// prose at column 0, top-level tasks at depth=0.
fn write_phase(out: &mut String, phase: &Phase) {
    if phase.id.is_empty() {
        out.push_str("## Phase\n");
    } else {
        let title_suffix = if phase.title.is_empty() {
            String::new()
        } else {
            format!(" - {}", phase.title)
        };
        let mut header = format!("## Phase {}{}", phase.id, title_suffix);
        if !phase.depends_on.is_empty() {
            header.push_str(&format!(" *(depends on: {})*", phase.depends_on.join(", ")));
        }
        if !phase.prefer_after.is_empty() {
            header.push_str(&format!(
                " *(prefer after: {})*",
                phase.prefer_after.join(", ")
            ));
        }
        header.push('\n');
        out.push_str(&header);
    }
    let inner = "";
    for ann in &phase.annotations {
        write_annotation(out, ann, inner);
    }
    for child in &phase.children {
        write_node(out, child, 0);
    }
}

fn write_node(out: &mut String, node: &Node, depth: usize) {
    let indent = " ".repeat(depth * 2);
    let mark = match node.state {
        NodeState::Done => "x",
        NodeState::WontDo => "-",
        NodeState::Backlog => ">",
        NodeState::Pending => " ",
    };
    // Phase 29.7: build the post-checkbox body explicitly so a bare-id leaf
    // (id == "") emits `- [ ] title`, not `- [ ]  title` (double-space).
    // FORMATv2 always renders id + title with the ` - ` hyphen-space separator.
    let body = if node.id.is_empty() {
        node.title.clone()
    } else {
        format!("{} - {}", node.id, node.title)
    };
    out.push_str(&format!("{indent}- [{mark}] {body}\n"));

    let inner = " ".repeat((depth + 1) * 2);
    for ann in &node.annotations {
        write_annotation(out, ann, &inner);
    }
    for child in &node.children {
        write_node(out, child, depth + 1);
    }
}

fn write_annotation(out: &mut String, ann: &Annotation, inner: &str) {
    match ann {
        Annotation::Text { text, indent } => {
            // Preserve the original indent from parse time. Narrative content
            // the user wrote at column 0 (`---` dividers, top-level prose,
            // `## Phase history` headers) must NOT be re-indented under
            // whatever checkbox the parser happened to attach it to.
            out.push_str(&" ".repeat(*indent));
            out.push_str(text);
            out.push('\n');
        }
        Annotation::Bullet { text, indent } => {
            // Phase 29.3: preserve the original indent for bullet annotations
            // too. Previously emitted at the parent's canonical depth + 2,
            // which destroyed user-formatted bullet lists in the preamble
            // (e.g. `## Phase history` one-liners). Bullets at column 0 in
            // source stay at column 0 in output.
            out.push_str(&" ".repeat(*indent));
            out.push_str("- ");
            out.push_str(text);
            out.push('\n');
            let _ = inner; // kept for signature symmetry with CodeBlock
        }
        Annotation::CodeBlock { lang, content, .. } => {
            out.push_str(inner);
            out.push_str("```");
            if let Some(l) = lang {
                out.push_str(l);
            }
            out.push('\n');
            out.push_str(content);
            if !content.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(inner);
            out.push_str("```\n");
        }
        Annotation::Blank { count } => {
            for _ in 0..*count {
                out.push('\n');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn roundtrip_stable(input: &str) {
        let plan1 = parse(input).expect("first parse");
        let out = serialize(&plan1);
        let plan2 = parse(&out).expect("reparse of serialized output");
        assert_eq!(plan1, plan2, "AST changed across serialize/parse roundtrip");
    }

    #[test]
    fn empty_plan() {
        assert_eq!(serialize(&Plan::default()), "");
    }

    #[test]
    fn single_unchecked_phase() {
        let plan = parse("## Phase 1 - Phase\n").unwrap();
        assert_eq!(serialize(&plan), "## Phase 1 - Phase\n");
    }

    #[test]
    fn single_checked_phase() {
        let plan = parse("## Phase 1 - Done\n- [x] 1.1 - Done task\n").unwrap();
        assert_eq!(
            serialize(&plan),
            "## Phase 1 - Done\n- [x] 1.1 - Done task\n"
        );
    }

    #[test]
    fn wont_do_phase_round_trips_with_dash() {
        let plan = parse("## Phase 1 - Skipped\n- [-] 1.1 - Skipped task\n").unwrap();
        assert_eq!(
            serialize(&plan),
            "## Phase 1 - Skipped\n- [-] 1.1 - Skipped task\n"
        );
    }

    #[test]
    fn backlog_phase_round_trips_with_gt() {
        let plan = parse("## Phase 1 - Deferred\n- [>] 1.1 - Deferred task\n").unwrap();
        assert_eq!(
            serialize(&plan),
            "## Phase 1 - Deferred\n- [>] 1.1 - Deferred task\n"
        );
    }

    #[test]
    fn tilde_input_normalizes_to_dash_on_write() {
        let plan = parse("## Phase 1 - Skipped\n- [~] 1.1 - Skipped task\n").unwrap();
        // Tilde is accepted on read, but canonical output is `[-]`.
        assert_eq!(
            serialize(&plan),
            "## Phase 1 - Skipped\n- [-] 1.1 - Skipped task\n"
        );
    }

    #[test]
    fn backlog_field_renders_at_bottom() {
        let mut plan = parse("## Phase 1 - Phase\n- [ ] 1.1 - Task\n").unwrap();
        plan.backlog
            .push("- **Deferred thing** — added 2026-05-19.".to_string());
        let out = serialize(&plan);
        assert_eq!(
            out,
            "## Phase 1 - Phase\n- [ ] 1.1 - Task\n\n## Backlog (not yet phased)\n\n- **Deferred thing** — added 2026-05-19.\n"
        );
    }

    #[test]
    fn empty_backlog_field_emits_nothing() {
        let plan = parse("## Phase 1 - Phase\n").unwrap();
        assert!(plan.backlog.is_empty());
        assert_eq!(serialize(&plan), "## Phase 1 - Phase\n");
    }

    #[test]
    fn backlog_only_plan_renders_without_leading_blank() {
        let mut plan = Plan::default();
        plan.backlog
            .push("- **Orphan note** — added 2026-05-19.".to_string());
        assert_eq!(
            serialize(&plan),
            "## Backlog (not yet phased)\n\n- **Orphan note** — added 2026-05-19.\n"
        );
    }

    #[test]
    fn nested_normalizes_indent() {
        let input = "\
## Phase 1 - Phase
    - [ ] 1.1 Task
        - [ ] 1.1.1 Sub
";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        let expected = "\
## Phase 1 - Phase
- [ ] 1.1 - Task
  - [ ] 1.1.1 - Sub
";
        assert_eq!(out, expected);
    }

    #[test]
    fn preserves_preamble() {
        let input = "\
# Header

Prose.

## Phase 1 - Phase
";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        assert!(out.starts_with("# Header\n\nProse.\n\n"));
        assert!(out.contains("## Phase 1 - Phase\n"));
    }

    #[test]
    fn roundtrips_basic_fixture() {
        let input = include_str!("../tests/fixtures/basic.md");
        roundtrip_stable(input);
    }

    #[test]
    fn roundtrips_with_annotations() {
        let input = "\
## Phase 1 - Phase
Some text annotation.
- a bullet annotation
- [ ] 1.1 - Task
";
        roundtrip_stable(input);
    }

    #[test]
    fn plain_id_round_trips() {
        let input = "## Phase 1 - Phase\n- [ ] 1.2.3 - Plain id\n";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        assert_eq!(out, input);
    }

    #[test]
    fn bare_id_leaf_serializes_without_double_space() {
        // Phase 29.7 regression. A bare-checkbox leaf (`- [ ] just a title`,
        // no id) used to round-trip as `- [ ]  just a title` (double space)
        // because the format string assumed an id was always present.
        let input = "## Phase 1 - Phase\n- [ ] Make the core domain model the source of truth.\n";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        assert_eq!(out, input, "bare-id leaf must round-trip cleanly:\n{out}");
        assert!(!out.contains("[ ]  "), "no double space:\n{out}");
    }

    #[test]
    fn blank_lines_inside_phase_tree_round_trip() {
        // Phase 29.6: blank lines inside a phase tree are captured as
        // `Annotation::Blank { count }` and re-emitted on serialize.
        let input = "\
## Phase 1 - Phase
- [ ] 1.1 - Task

- [ ] 1.2 - Task after blank
";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        assert_eq!(out, input, "blank inside tree should round-trip:\n{out}");
    }

    #[test]
    fn consecutive_blanks_coalesce_and_round_trip() {
        let input = "\
## Phase 1 - Phase
- [ ] 1.1 - Task



- [ ] 1.2 - After 3 blanks
";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        assert_eq!(out, input, "3 blanks should round-trip:\n{out}");
    }

    #[test]
    fn hyphen_separator_round_trips() {
        // FORMATv2 canonical separator is ` - ` (hyphen-space).
        let input = "## Phase 1 - Phase\n- [x] 1.2.3 - Hyphen-separated title\n";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        assert_eq!(out, input, "hyphen should round-trip:\n{out}");
    }

    #[test]
    fn bullet_annotations_preserve_original_indent_at_column_zero() {
        // Phase 29.3 regression. The dry-run on a large adopter's PLAN.md
        // showed every `- **Phase N** — ...` bullet under `## Phase history`
        // (a column-0 narrative bullet attached to whichever node the parser
        // attached it to) getting re-emitted at column 2. Bullets must
        // preserve their original indent — not snap to the parent node's
        // canonical depth.
        let input = "\
## Phase 1 - Phase
- [ ] 1.1 - task

## Phase history

- **Phase N** — done.
- **Phase O** — done.

## Phase 2 - Next phase
- [ ] 2.1 - task
";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        assert!(
            out.contains("\n- **Phase N** — done.\n"),
            "column-0 bullet should stay at column 0:\n{out}"
        );
        assert!(
            !out.contains("  - **Phase N**"),
            "bullet must NOT be indented to canonical depth:\n{out}"
        );
    }

    #[test]
    fn roundtrips_with_code_block() {
        let input = "\
## Phase 1 - Phase
```rust
fn foo() {}
```
";
        roundtrip_stable(input);
    }

    #[test]
    fn renders_annotations_at_correct_depth() {
        let input = "\
## Phase 1 - Phase
- [ ] 1.1 - Task
  text annotation on 1.1
";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        // Annotation on 1.1 (a column-0 task) should be at 2-space indent.
        assert!(out.contains("  text annotation on 1.1\n"), "got:\n{out}");
    }

    #[test]
    fn preserves_horizontal_rule_at_original_column() {
        // Phase 21.1 — quicksight shakeout. `---` at column 0 between phases
        // attached as annotation on the previous phase; on re-serialize it
        // used to be demoted to canonical-child indent (4+ spaces). Now it
        // stays at column 0.
        let input = "\
## Phase 1 - First
- [ ] 1.1 - sub
---
## Phase 2 - Second
- [ ] 2.1 - sub
";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        // The `---` should be at column 0, not indented.
        let mut found = false;
        for line in out.lines() {
            if line.trim() == "---" {
                assert!(
                    !line.starts_with(' '),
                    "--- got demoted to indented: {line:?}"
                );
                found = true;
            }
        }
        assert!(found, "--- preserved somewhere in output:\n{out}");
    }

    #[test]
    fn preserves_blanks_between_top_level_phases_when_present_in_source() {
        // Phase 29.6: blanks come from source via `Annotation::Blank`, not
        // serializer auto-insertion. When source has blanks between phases,
        // they round-trip. When source has none, none are emitted.
        let with_blanks = parse("## Phase 1 - A\n\n## Phase 2 - B\n\n## Phase 3 - C\n").unwrap();
        let out = serialize(&with_blanks);
        assert!(
            out.contains("## Phase 1 - A\n\n## Phase 2 - B\n"),
            "blank between 1.0 and 2.0 preserved from source:\n{out}"
        );
        assert!(
            out.contains("## Phase 2 - B\n\n## Phase 3 - C\n"),
            "blank between 2.0 and 3.0 preserved from source:\n{out}"
        );

        let no_blanks = parse("## Phase 1 - A\n## Phase 2 - B\n").unwrap();
        let out_no = serialize(&no_blanks);
        assert!(
            !out_no.contains("\n\n## Phase 2 - B"),
            "no blank between phases when source had none:\n{out_no}"
        );
    }

    // -----------------------------------------------------------------
    // Phase 37: FORMATv2 serializer dispatch
    // -----------------------------------------------------------------

    #[test]
    fn v2_phase_emits_header_form_with_both_markers() {
        // A round-trip via parse: `## Phase AS - Title *(depends on: AR)*
        // *(prefer after: AB)*` → Phase with source=HeaderV2 + markers →
        // re-emit yields the same header line.
        let input = "\
## Phase AS - Spine *(depends on: AR, AQ)* *(prefer after: AB)*

- [ ] AS.0 plan
";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        assert!(
            out.contains("## Phase AS - Spine *(depends on: AR, AQ)* *(prefer after: AB)*"),
            "v2 header with both markers round-trips:\n{out}"
        );
        // Top-level task lands at column 0 (depth=0) under a v2 phase, with the
        // canonical ` - ` separator.
        assert!(
            out.contains("\n- [ ] AS.0 - plan\n"),
            "task at column 0:\n{out}"
        );
        // And the phase itself stays a header — no checkbox anchor for it.
        assert!(
            !out.contains("- [ ] AS - Spine"),
            "phase must not render as an anchor checkbox:\n{out}"
        );
    }

    #[test]
    fn v2_phase_emits_header_form_with_tasks_at_column_zero() {
        // A v2 header phase always serializes via `## Phase X - Title` with its
        // tasks dedented to column 0 and the ` - ` separator — never an anchor
        // checkbox for the phase.
        let input = "## Phase 1 - Done legacy phase\n- [x] 1.1 - task\n";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        assert_eq!(out, input, "v2 header round-trips canonically:\n{out}");
    }

    #[test]
    fn mixed_phases_serialize_each_as_header() {
        // Two v2 header phases: each round-trips as a header with column-0
        // tasks. The fixture covers this end-to-end; this is the minimal
        // direct assertion.
        let input = "\
## Phase 1 - Legacy
- [x] 1.1 - done

## Phase AI - New world

- [ ] AI.0 - task
";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        assert!(
            out.contains("## Phase 1 - Legacy"),
            "phase 1 header preserved"
        );
        assert!(
            out.contains("## Phase AI - New world"),
            "v2 header preserved"
        );
        assert!(out.contains("\n- [ ] AI.0 - task\n"), "AI.0 at column 0");
        assert!(out.contains("- [x] 1.1 - done"), "phase-1 task at column 0");
    }

    #[test]
    fn no_blank_before_first_phase_or_after_last() {
        let plan = parse("## Phase 1 - Only\n").unwrap();
        let out = serialize(&plan);
        assert!(
            !out.starts_with('\n'),
            "shouldn't start with blank: {out:?}"
        );
        // Single trailing newline is fine; multiple would be excess.
        assert!(!out.ends_with("\n\n\n"), "excess trailing blanks: {out:?}");
    }
}

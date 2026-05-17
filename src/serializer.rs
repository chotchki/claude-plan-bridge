use crate::ast::{Annotation, Node, NodeState, Plan};

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
    for (i, phase) in plan.phases.iter().enumerate() {
        // Blank line between top-level phases — matches the archive section
        // build and gives a long PLAN.md visual breathing room. Inside a
        // phase tree the parser drops blanks anyway.
        if i > 0 {
            out.push('\n');
        }
        write_node(&mut out, phase, 0);
    }
    out
}

fn write_node(out: &mut String, node: &Node, depth: usize) {
    let indent = " ".repeat(depth * 2);
    let mark = match node.state {
        NodeState::Done => "x",
        NodeState::WontDo => "-",
        NodeState::Pending => " ",
    };
    out.push_str(&format!("{indent}- [{mark}] {} {}\n", node.id, node.title));

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
            // Text annotations always preserve their *original* indent from
            // parse time. Otherwise narrative content the user wrote at
            // column 0 (`---` dividers, top-level prose, `## Phase history`
            // headers) gets re-emitted indented under whatever checkbox the
            // parser happened to attach it to. Bullets and code-blocks keep
            // canonical-depth indent below (their meaning is structural).
            out.push_str(&" ".repeat(*indent));
            out.push_str(text);
            out.push('\n');
        }
        Annotation::Bullet { text, .. } => {
            out.push_str(inner);
            out.push_str("- ");
            out.push_str(text);
            out.push('\n');
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
        let plan = parse("- [ ] 1.0 Phase\n").unwrap();
        assert_eq!(serialize(&plan), "- [ ] 1.0 Phase\n");
    }

    #[test]
    fn single_checked_phase() {
        let plan = parse("- [x] 1.0 Done\n").unwrap();
        assert_eq!(serialize(&plan), "- [x] 1.0 Done\n");
    }

    #[test]
    fn wont_do_phase_round_trips_with_dash() {
        let plan = parse("- [-] 1.0 Skipped\n").unwrap();
        assert_eq!(serialize(&plan), "- [-] 1.0 Skipped\n");
    }

    #[test]
    fn tilde_input_normalizes_to_dash_on_write() {
        let plan = parse("- [~] 1.0 Skipped\n").unwrap();
        // Tilde is accepted on read, but canonical output is `[-]`.
        assert_eq!(serialize(&plan), "- [-] 1.0 Skipped\n");
    }

    #[test]
    fn nested_normalizes_indent() {
        let input = "\
- [ ] 1.0 Phase
    - [ ] 1.1 Task
        - [ ] 1.1.1 Sub
";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        let expected = "\
- [ ] 1.0 Phase
  - [ ] 1.1 Task
    - [ ] 1.1.1 Sub
";
        assert_eq!(out, expected);
    }

    #[test]
    fn preserves_preamble() {
        let input = "\
# Header

Prose.

- [ ] 1.0 Phase
";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        assert!(out.starts_with("# Header\n\nProse.\n\n"));
        assert!(out.contains("- [ ] 1.0 Phase\n"));
    }

    #[test]
    fn roundtrips_basic_fixture() {
        let input = include_str!("../tests/fixtures/basic.md");
        roundtrip_stable(input);
    }

    #[test]
    fn roundtrips_with_annotations() {
        let input = "\
- [ ] 1.0 Phase
  Some text annotation.
  - a bullet annotation
  - [ ] 1.1 Task
";
        roundtrip_stable(input);
    }

    #[test]
    fn roundtrips_with_code_block() {
        let input = "\
- [ ] 1.0 Phase
  ```rust
  fn foo() {}
  ```
";
        roundtrip_stable(input);
    }

    #[test]
    fn renders_annotations_at_correct_depth() {
        let input = "\
- [ ] 1.0 Phase
  - [ ] 1.1 Task
    text annotation on 1.1
";
        let plan = parse(input).unwrap();
        let out = serialize(&plan);
        // Annotation on 1.1 (depth 1) should be at 4-space indent.
        assert!(out.contains("    text annotation on 1.1\n"), "got:\n{out}");
    }

    #[test]
    fn preserves_horizontal_rule_at_original_column() {
        // Phase 21.1 — quicksight shakeout. `---` at column 0 between phases
        // attached as annotation on the previous phase; on re-serialize it
        // used to be demoted to canonical-child indent (4+ spaces). Now it
        // stays at column 0.
        let input = "\
- [ ] 1.0 First
  - [ ] 1.1 sub
---
- [ ] 2.0 Second
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
    fn inserts_blank_line_between_top_level_phases() {
        // Phase 21.2 — readability nit. Source files commonly have blank
        // lines between top-level phases; parser drops them. Now the
        // serializer puts one back.
        let plan = parse("- [ ] 1.0 A\n- [ ] 2.0 B\n- [ ] 3.0 C\n").unwrap();
        let out = serialize(&plan);
        // Each `- [ ] N.0 X\n` line should be followed (except last) by
        // exactly one blank line before the next phase header.
        assert!(
            out.contains("- [ ] 1.0 A\n\n- [ ] 2.0 B\n"),
            "blank between 1.0 and 2.0:\n{out}"
        );
        assert!(
            out.contains("- [ ] 2.0 B\n\n- [ ] 3.0 C\n"),
            "blank between 2.0 and 3.0:\n{out}"
        );
    }

    #[test]
    fn no_blank_before_first_phase_or_after_last() {
        let plan = parse("- [ ] 1.0 Only\n").unwrap();
        let out = serialize(&plan);
        assert!(!out.starts_with('\n'), "shouldn't start with blank: {out:?}");
        // Single trailing newline is fine; multiple would be excess.
        assert!(!out.ends_with("\n\n\n"), "excess trailing blanks: {out:?}");
    }
}

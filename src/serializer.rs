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
    for phase in &plan.phases {
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
            // Markdown headers preserve their *original* indent from parse
            // time — otherwise a `## Phase history` at column 0 in source
            // would be re-emitted at the canonical child indent, visually
            // demoting top-level narrative under whatever checkbox the
            // parser happened to attach it to.
            let actual = if crate::ast::looks_like_markdown_header(text) {
                " ".repeat(*indent)
            } else {
                inner.to_string()
            };
            out.push_str(&actual);
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
}

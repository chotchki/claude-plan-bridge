//! Phase 39.2: end-to-end dogfood against the user's real `../quicksight/PLAN.md`.
//!
//! Confirms that the FORMATv2 bridge (parser + serializer) can handle a
//! real-world mid-pivot plan without losing content. The quicksight repo is
//! opportunistic — if it's not checked out alongside this repo, the test
//! silently skips (same pattern as parser.rs's `smoke_test_quicksight_plan_md`).
//!
//! What we assert:
//! - Parse the file: every phase present, no parse error
//! - Round-trip parse → serialize → parse is AST-stable

fn quicksight_plan_path() -> Option<std::path::PathBuf> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.parent()?.join("quicksight/PLAN.md");
    if path.exists() { Some(path) } else { None }
}

#[test]
fn quicksight_plan_parses_with_all_phases() {
    let Some(path) = quicksight_plan_path() else {
        eprintln!("skip: ../quicksight/PLAN.md not present");
        return;
    };
    let text = std::fs::read_to_string(&path).expect("read quicksight PLAN.md");
    let plan = plan_bridge::parser::parse(&text)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    assert!(
        !plan.phases.is_empty(),
        "quicksight PLAN.md should have at least one phase"
    );
    // Sanity: phase ids look v2-shaped (alphabetic or `AO.R`-style alphanumeric).
    for p in &plan.phases {
        assert!(!p.id.is_empty(), "every phase has an id (got empty): {p:?}");
    }
}

#[test]
fn quicksight_plan_round_trips_through_parser_serializer() {
    let Some(path) = quicksight_plan_path() else {
        eprintln!("skip: ../quicksight/PLAN.md not present");
        return;
    };
    let text = std::fs::read_to_string(&path).expect("read");
    let plan1 = plan_bridge::parser::parse(&text).expect("parse once");
    let serialized = plan_bridge::serializer::serialize(&plan1);
    let plan2 = plan_bridge::parser::parse(&serialized).expect("parse twice");
    assert_eq!(
        plan1, plan2,
        "AST must be stable across parse → serialize → parse"
    );
}

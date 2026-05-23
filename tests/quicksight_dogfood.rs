//! Phase 39.2: end-to-end dogfood against the user's real `../quicksight/PLAN.md`.
//!
//! Confirms that the FORMATv2 bridge (parser + serializer + canonicalize) can
//! handle a real-world mid-pivot plan without losing content. The quicksight
//! repo is opportunistic — if it's not checked out alongside this repo, the
//! test silently skips (same pattern as parser.rs's `smoke_test_quicksight_plan_md`).
//!
//! What we assert:
//! - Parse the file: every phase present, no parse error
//! - Canonicalize (in scratchdir copy): doesn't error, doesn't change the
//!   structural leaf count
//! - Parse the canonicalized form: same set of phase ids, same set of task
//!   ids — round-trip preserves the work
//! - Phase deps survive canonicalize (depends_on / prefer_after lists intact)

use std::path::Path;

fn quicksight_plan_path() -> Option<std::path::PathBuf> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.parent()?.join("quicksight/PLAN.md");
    if path.exists() { Some(path) } else { None }
}

fn collect_all_ids(plan: &plan_bridge::ast::Plan) -> Vec<String> {
    let mut ids = Vec::new();
    for phase in &plan.phases {
        ids.push(phase.id.clone());
        collect_node_ids(&phase.children, &mut ids);
    }
    ids.sort();
    ids
}

fn collect_node_ids(nodes: &[plan_bridge::ast::Node], out: &mut Vec<String>) {
    for node in nodes {
        out.push(node.id.clone());
        collect_node_ids(&node.children, out);
    }
}

fn collect_dep_metadata(
    plan: &plan_bridge::ast::Plan,
) -> Vec<(String, Vec<String>, Vec<String>)> {
    plan.phases
        .iter()
        .map(|p| (p.id.clone(), p.depends_on.clone(), p.prefer_after.clone()))
        .collect()
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
        assert!(
            !p.id.is_empty(),
            "every phase has an id (got empty): {p:?}"
        );
    }
}

#[test]
fn quicksight_plan_canonicalize_preserves_ids() {
    let Some(src) = quicksight_plan_path() else {
        eprintln!("skip: ../quicksight/PLAN.md not present");
        return;
    };

    // Copy to a scratch dir — canonicalize writes back, and we don't want
    // to mutate the user's real file.
    let scratch = plan_bridge::test_utils::scratch_dir("quicksight-dogfood");
    let dst: std::path::PathBuf = scratch.join("PLAN.md");
    std::fs::copy(&src, &dst).expect("copy quicksight PLAN.md to scratch");

    let original_text = std::fs::read_to_string(&dst).unwrap();
    let original_plan = plan_bridge::parser::parse(&original_text).expect("parse pre");
    let ids_before = collect_all_ids(&original_plan);
    let deps_before = collect_dep_metadata(&original_plan);

    let report = plan_bridge::canonicalize::canonicalize(&dst, false)
        .expect("canonicalize quicksight copy");

    // Canonicalize is allowed to be a no-op (already v2) OR mutate. Either is
    // fine; what matters is round-trip preserves content.
    let after_text = std::fs::read_to_string(&dst).unwrap();
    let after_plan = plan_bridge::parser::parse(&after_text)
        .expect("parse post-canonicalize");
    let ids_after = collect_all_ids(&after_plan);
    let deps_after = collect_dep_metadata(&after_plan);

    assert_eq!(
        ids_before, ids_after,
        "canonicalize must not drop or rename any phase/task id\n\
         notes: {:?}",
        report.notes
    );
    assert_eq!(
        deps_before, deps_after,
        "canonicalize must preserve every phase's depends_on / prefer_after"
    );

    cleanup(&scratch);
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

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

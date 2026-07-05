//! Phase CI: end-to-end coverage of `promote --into` / `--after` — filing a
//! backlog entry as a TASK under an existing phase or task, driven through the
//! real built binary against scratch PLAN.md files. Also asserts the
//! "TaskCreate too" contract: the promoted leaves surface through `reconcile`
//! so the harness picks them up on the next turn.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch_dir() -> PathBuf {
    plan_bridge::test_utils::scratch_dir("promote-into")
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_claude-plan-bridge"))
}

fn run(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(binary())
        .args(args)
        .arg("--cwd")
        .arg(dir)
        .output()
        .unwrap_or_else(|e| panic!("spawn {args:?}: {e}"));
    assert!(
        out.status.success(),
        "command {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn write_plan(dir: &Path, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("PLAN.md"), body).unwrap();
}

fn plan_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("PLAN.md")).unwrap()
}

const PLAN_WITH_BACKLOG: &str = "\
## Phase CE - existing phase
- [ ] CE.1 - first task
- [ ] CE.2 - second task

# Backlog (not yet phased)

- A simple loose idea
- X.1 - Descoped parent *(deferred from phase `X` on 2026-01-01)*
  - X.1.1 - descoped child
  - X.1.2 - another child
";

#[test]
fn after_inserts_alpha_suffix_leaf_between_siblings() {
    let dir = scratch_dir();
    write_plan(&dir, PLAN_WITH_BACKLOG);

    let out = run(&dir, &["promote", "1", "--into", "CE", "--after", "CE.1"]);
    assert!(out.contains("as `CE.1a`"), "reported new id: {out}");
    assert!(out.contains("single task"), "fallback leaf noted: {out}");

    let plan = plan_text(&dir);
    // Wedged between CE.1 and CE.2, no renumbering.
    let a = plan.find("CE.1 - first task").unwrap();
    let b = plan.find("CE.1a - A simple loose idea").unwrap();
    let c = plan.find("CE.2 - second task").unwrap();
    assert!(a < b && b < c, "ordering CE.1 < CE.1a < CE.2:\n{plan}");
}

#[test]
fn faithful_subtree_remaps_ids_and_strips_marker() {
    let dir = scratch_dir();
    write_plan(&dir, PLAN_WITH_BACKLOG);

    // Entry 2 is the descoped subtree.
    let out = run(&dir, &["promote", "2", "--into", "CE"]);
    assert!(out.contains("reconstructed 3 task(s)"), "faithful: {out}");
    assert!(
        out.contains("CE.3, CE.3.1, CE.3.2"),
        "remapped ids listed: {out}"
    );

    let plan = plan_text(&dir);
    assert!(plan.contains("- [ ] CE.3 - Descoped parent"), "marker stripped:\n{plan}");
    assert!(plan.contains("  - [ ] CE.3.1 - descoped child"), "child 1:\n{plan}");
    assert!(plan.contains("  - [ ] CE.3.2 - another child"), "child 2:\n{plan}");
    // Old foreign ids are gone.
    assert!(!plan.contains("X.1.1"), "old ids remapped away:\n{plan}");
    // Backlog stanza drained; the simple idea remains as the sole entry.
    let listed = run(&dir, &["promote"]);
    assert!(listed.contains("A simple loose idea"), "simple idea remains: {listed}");
    assert!(!listed.contains("Descoped parent"), "subtree drained: {listed}");
}

#[test]
fn into_a_task_nests_and_activate_scopes_the_phase() {
    let dir = scratch_dir();
    write_plan(
        &dir,
        "## Phase CE - x\n- [ ] CE.3 - parent\n  - [ ] CE.3.1 - existing\n\n\
         # Backlog (not yet phased)\n\n- nested idea\n",
    );
    let out = run(&dir, &["promote", "1", "--into", "CE.3", "--activate"]);
    assert!(out.contains("as `CE.3.2`"), "nested under the task: {out}");
    assert!(out.contains("activated `CE`"), "activated the root phase: {out}");
    assert!(plan_text(&dir).contains("  - [ ] CE.3.2 - nested idea"));
}

#[test]
fn promoted_leaves_surface_for_taskcreate_in_active_phase() {
    let dir = scratch_dir();
    write_plan(&dir, PLAN_WITH_BACKLOG);
    run(&dir, &["activate", "CE"]);
    // Baseline maps the pre-existing leaves so reconcile's diff isolates the
    // freshly-promoted ones.
    run(&dir, &["baseline"]);

    run(&dir, &["promote", "2", "--into", "CE"]);
    let recon = run(&dir, &["reconcile"]);

    assert!(recon.contains("Active phase `CE` drift:"), "foregrounded: {recon}");
    // Every promoted leaf — not just the headline — is offered for TaskCreate.
    for id in ["CE.3", "CE.3.1", "CE.3.2"] {
        assert!(
            recon.contains(id) && recon.contains("consider TaskCreate"),
            "expected `{id}` surfaced for TaskCreate:\n{recon}"
        );
    }
}

#[test]
fn promote_into_non_active_phase_surfaces_as_other() {
    let dir = scratch_dir();
    write_plan(
        &dir,
        "## Phase CE - active\n- [ ] CE.1 - a\n## Phase CF - dormant\n- [ ] CF.1 - b\n\n\
         # Backlog (not yet phased)\n\n- X.1 - Descoped *(deferred from phase `X` on 2026-01-01)*\n",
    );
    run(&dir, &["activate", "CE"]);
    run(&dir, &["baseline"]);

    run(&dir, &["promote", "1", "--into", "CF"]);
    let recon = run(&dir, &["reconcile"]);

    // Still tracked (consider TaskCreate), but under the non-foreground bucket
    // since CF isn't the active phase.
    assert!(recon.contains("CF.2"), "new leaf surfaced: {recon}");
    assert!(recon.contains("consider TaskCreate"), "still offered: {recon}");
    assert!(
        recon.contains("Other phases / cross-cutting:"),
        "bucketed as non-active: {recon}"
    );
}

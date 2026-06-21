//! Phase CD: end-to-end coverage of the self-sustaining planning loop through
//! the real `reconcile` CLI — auto-advance + status-on-change heartbeat,
//! including the dedupe that keeps them quiet on no-change turns. Drives the
//! built binary against scratch PLAN.md files.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch_dir() -> PathBuf {
    plan_bridge::test_utils::scratch_dir("planning-loop")
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

#[test]
fn auto_advance_nudge_fires_once_through_reconcile() {
    let dir = scratch_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("PLAN.md"),
        "## Phase CB - One\n- [x] CB.1 - a\n## Phase CC - Two\n- [ ] CC.1 - b\n",
    )
    .unwrap();
    run(&dir, &["activate", "CB"]);

    let first = run(&dir, &["reconcile"]);
    assert!(
        first.contains("Phase CB is complete")
            && first.contains("archive CB")
            && first.contains("plan_activate CC"),
        "expected the auto-advance nudge: {first}"
    );

    // Dedupe: the completed-but-unarchived phase does not nudge again.
    let second = run(&dir, &["reconcile"]);
    assert!(
        !second.contains("Phase CB is complete"),
        "auto-advance must fire once, not every prompt: {second}"
    );
}

#[test]
fn heartbeat_fires_on_change_and_dedupes_through_reconcile() {
    let dir = scratch_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("PLAN.md"),
        "## Phase CB - One\n- [x] CB.1 - a\n- [ ] CB.2 - b\n- [ ] CB.3 - c\n",
    )
    .unwrap();
    run(&dir, &["activate", "CB"]);

    let first = run(&dir, &["reconcile"]);
    assert!(
        first.contains("active: CB (1/3 done)"),
        "expected a heartbeat: {first}"
    );

    // No change -> silent (the whole point: not its own noise).
    let second = run(&dir, &["reconcile"]);
    assert!(
        !second.contains("active: CB"),
        "unchanged progress must stay quiet: {second}"
    );

    // Progress changes -> re-fires with the new count.
    std::fs::write(
        dir.join("PLAN.md"),
        "## Phase CB - One\n- [x] CB.1 - a\n- [x] CB.2 - b\n- [ ] CB.3 - c\n",
    )
    .unwrap();
    let third = run(&dir, &["reconcile"]);
    assert!(
        third.contains("active: CB (2/3 done)"),
        "a tick re-fires the heartbeat: {third}"
    );
}

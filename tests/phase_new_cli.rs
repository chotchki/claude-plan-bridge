//! Phase CE: end-to-end coverage for `phase-new` + `phase-breakdown` through
//! the built binary — templated phase creation, recursive/repeatable
//! breakdown, and the optional `PHASE_TEMPLATE.md` override.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch_dir() -> PathBuf {
    plan_bridge::test_utils::scratch_dir("phase-new-cli")
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
fn phase_new_then_recursive_breakdown() {
    let dir = scratch_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("PLAN.md"),
        "# PLAN\n## Phase BY - prior\n- [x] BY.1 - x\n",
    )
    .unwrap();

    // phase-new auto-assigns BZ and applies the built-in 5-beat default.
    run(&dir, &["phase-new", "Demo"]);
    let plan = std::fs::read_to_string(dir.join("PLAN.md")).unwrap();
    assert!(plan.contains("## Phase BZ - Demo"), "{plan}");
    assert!(plan.contains("- [ ] BZ.1 - Plan & breakdown"), "{plan}");
    assert!(plan.contains("- [ ] BZ.5 - Release"), "{plan}");

    // Break the Implement beat into children, then one of those again (recursive).
    run(&dir, &["phase-breakdown", "BZ.2", "--tasks", "codec,scan"]);
    run(&dir, &["phase-breakdown", "BZ.2.1", "--tasks", "deep"]);
    // Repeat on BZ.2 — appends after the existing children.
    run(&dir, &["phase-breakdown", "BZ.2", "--tasks", "docs"]);

    let plan = std::fs::read_to_string(dir.join("PLAN.md")).unwrap();
    assert!(plan.contains("- [ ] BZ.2.1 - codec"), "{plan}");
    assert!(plan.contains("- [ ] BZ.2.2 - scan"), "{plan}");
    assert!(
        plan.contains("- [ ] BZ.2.1.1 - deep"),
        "recursive depth:\n{plan}"
    );
    assert!(
        plan.contains("- [ ] BZ.2.3 - docs"),
        "repeat appended:\n{plan}"
    );
}

#[test]
fn phase_new_honors_project_template() {
    let dir = scratch_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("PLAN.md"), "# PLAN\n").unwrap();
    std::fs::write(
        dir.join("PHASE_TEMPLATE.md"),
        "# Template\n- Spike\n- Build\n",
    )
    .unwrap();

    run(&dir, &["phase-new", "Custom"]);
    let plan = std::fs::read_to_string(dir.join("PLAN.md")).unwrap();
    // Fresh project -> first id is A.
    assert!(plan.contains("## Phase A - Custom"), "{plan}");
    assert!(plan.contains("- [ ] A.1 - Spike"), "{plan}");
    assert!(plan.contains("- [ ] A.2 - Build"), "{plan}");
    assert!(!plan.contains("A.3"), "template had only 2 beats:\n{plan}");
}

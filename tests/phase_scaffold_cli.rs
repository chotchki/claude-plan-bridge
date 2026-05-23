//! Phase 40.7: integration test for the `phase-scaffold` CLI subcommand.
//! Invokes the built binary against scratch PLAN.md files and asserts the
//! on-disk shape.

use std::path::PathBuf;
use std::process::Command;

fn scratch_dir() -> PathBuf {
    plan_bridge::test_utils::scratch_dir("scaffold")
}

fn binary() -> PathBuf {
    // `cargo test` builds the binary alongside the test deps under
    // CARGO_TARGET_DIR (or the workspace target/debug). Resolve by
    // probing the workspace target directory.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidate = manifest.join("target/debug/claude-plan-bridge");
    if candidate.exists() {
        return candidate;
    }
    panic!(
        "claude-plan-bridge binary not built — run `cargo build` first (looked for {})",
        candidate.display()
    );
}

#[test]
fn phase_scaffold_creates_phase_with_tasks() {
    let dir = scratch_dir();
    let plan = dir.join("PLAN.md");
    std::fs::write(&plan, "# Project\n\n").unwrap();

    let output = Command::new(binary())
        .args([
            "phase-scaffold",
            "AT",
            "Spike a new direction",
            "--tasks",
            "0:Lock decisions,1:Audit current state,2:Build driver",
            "--plan",
        ])
        .arg(&plan)
        .output()
        .expect("run phase-scaffold");

    assert!(
        output.status.success(),
        "scaffold failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    let contents = std::fs::read_to_string(&plan).unwrap();
    assert!(
        contents.contains("## Phase AT - Spike a new direction"),
        "header landed: \n{contents}"
    );
    assert!(
        contents.contains("- [ ] AT.0 - Lock decisions"),
        "task 0 landed: \n{contents}"
    );
    assert!(
        contents.contains("- [ ] AT.1 - Audit current state"),
        "task 1 landed: \n{contents}"
    );
    assert!(
        contents.contains("- [ ] AT.2 - Build driver"),
        "task 2 landed: \n{contents}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase_scaffold_with_deps_emits_markers() {
    let dir = scratch_dir();
    let plan = dir.join("PLAN.md");
    std::fs::write(&plan, "# Project\n\n").unwrap();

    let output = Command::new(binary())
        .args([
            "phase-scaffold",
            "AS",
            "Spine",
            "--tasks",
            "0:plan",
            "--depends-on",
            "AR,AQ",
            "--prefer-after",
            "AB",
            "--plan",
        ])
        .arg(&plan)
        .output()
        .expect("run");

    assert!(output.status.success());
    let contents = std::fs::read_to_string(&plan).unwrap();
    assert!(
        contents.contains("## Phase AS - Spine *(depends on: AR, AQ)* *(prefer after: AB)*"),
        "header with both markers: \n{contents}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn phase_scaffold_refuses_existing_phase() {
    let dir = scratch_dir();
    let plan = dir.join("PLAN.md");
    std::fs::write(&plan, "## Phase AI - Existing\n\n- [ ] AI.0 task\n").unwrap();

    let output = Command::new(binary())
        .args([
            "phase-scaffold",
            "AI",
            "Duplicate",
            "--tasks",
            "0:nope",
            "--plan",
        ])
        .arg(&plan)
        .output()
        .expect("run");

    assert!(!output.status.success(), "should refuse duplicate");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("AI") && err.contains("already exists"),
        "error message names the duplicate: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

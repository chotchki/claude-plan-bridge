//! Phase 40.7: integration test for the `phase-scaffold` CLI subcommand.
//! Invokes the built binary against scratch PLAN.md files and asserts the
//! on-disk shape.

use std::path::PathBuf;
use std::process::Command;

fn scratch_dir() -> PathBuf {
    plan_bridge::test_utils::scratch_dir("scaffold")
}

fn binary() -> PathBuf {
    // Cargo sets CARGO_BIN_EXE_<name> for integration tests, pointing at
    // the built binary regardless of target dir (works under `cargo test`
    // and `cargo llvm-cov`, which uses target/llvm-cov-target).
    PathBuf::from(env!("CARGO_BIN_EXE_claude-plan-bridge"))
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

/// BY.4: `plan_activate` / `plan_deactivate` are accepted as clap aliases for
/// `activate` / `deactivate`, so the CLI verb matches the MCP tool names and
/// the wording the bridge emits in hook output + global CLAUDE.md. Driven
/// through the real binary so the `visible_alias` wiring is exercised.
#[test]
fn plan_activate_deactivate_aliases_drive_activate_deactivate() {
    let dir = scratch_dir();
    let plan = dir.join("PLAN.md");
    std::fs::write(&plan, "## Phase AS - Build\n\n- [ ] AS.1 - Task one\n").unwrap();

    let activate = Command::new(binary())
        .args(["plan_activate", "--plan"])
        .arg(&plan)
        .arg("AS")
        .output()
        .expect("run plan_activate");
    assert!(
        activate.status.success(),
        "plan_activate alias failed: stderr={}",
        String::from_utf8_lossy(&activate.stderr)
    );
    let stdout = String::from_utf8_lossy(&activate.stdout);
    assert!(stdout.contains("activated phase `AS`"), "got: {stdout}");

    let deactivate = Command::new(binary())
        .args(["plan_deactivate", "--plan"])
        .arg(&plan)
        .output()
        .expect("run plan_deactivate");
    assert!(
        deactivate.status.success(),
        "plan_deactivate alias failed: stderr={}",
        String::from_utf8_lossy(&deactivate.stderr)
    );
    let stdout = String::from_utf8_lossy(&deactivate.stdout);
    assert!(stdout.contains("deactivated focus"), "got: {stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

//! Phase BZ: integration coverage for the `next-phase` CLI subcommand.
//! Drives the built binary against scratch PLAN.md / PLAN_ARCHIVE.md files.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch_dir() -> PathBuf {
    plan_bridge::test_utils::scratch_dir("next-phase")
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_claude-plan-bridge"))
}

fn next_phase(dir: &Path) -> String {
    let out = Command::new(binary())
        .args(["next-phase", "--cwd"])
        .arg(dir)
        .output()
        .expect("run next-phase");
    assert!(
        out.status.success(),
        "next-phase failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn next_phase_cli_reconstructs_from_plan_and_archive() {
    let dir = scratch_dir();
    std::fs::create_dir_all(&dir).unwrap();

    // Fresh project with no alpha phases yet → start the sequence at A.
    std::fs::write(dir.join("PLAN.md"), "# PLAN\n- [ ] 1.0 nothing alpha\n").unwrap();
    assert_eq!(next_phase(&dir), "A");

    // Live BZ, with CA already swept to the archive: the next id must clear
    // BOTH the live plan and the archive, so CB — never the archived CA.
    std::fs::write(
        dir.join("PLAN.md"),
        "# PLAN\n## Phase BZ - live work\n- [ ] BZ.1 - a task\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("PLAN_ARCHIVE.md"),
        "## Phase BY - swept\n## Phase CA - swept\n",
    )
    .unwrap();
    assert_eq!(next_phase(&dir), "CB");

    // Legacy numeric phase ids are ignored; the alpha high-water mark wins.
    std::fs::write(
        dir.join("PLAN.md"),
        "## Phase 42 - legacy numeric\n## Phase BY - alpha\n",
    )
    .unwrap();
    std::fs::remove_file(dir.join("PLAN_ARCHIVE.md")).unwrap();
    assert_eq!(next_phase(&dir), "BZ");
}

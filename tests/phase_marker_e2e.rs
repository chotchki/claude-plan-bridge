//! Phase CJ: end-to-end coverage of the phase high-water marker and the
//! garbled-id cap through the built binary. Drives `next-phase`, `baseline`,
//! and `archive` against scratch PLAN.md / PLAN_ARCHIVE.md files and asserts
//! the two headline properties:
//!   1. a garbled / concatenated header (`## Phase CICJ`) can't poison next-id;
//!   2. once a marker exists, `next-phase` no longer scrapes PLAN_ARCHIVE.md.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch_dir() -> PathBuf {
    plan_bridge::test_utils::scratch_dir("phase-marker-e2e")
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_claude-plan-bridge"))
}

fn run(args: &[&str], dir: &Path) -> std::process::Output {
    Command::new(binary())
        .args(args)
        .arg("--cwd")
        .arg(dir)
        .output()
        .expect("run bridge")
}

fn next_phase(dir: &Path) -> String {
    let out = run(&["next-phase"], dir);
    assert!(
        out.status.success(),
        "next-phase failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn next_phase_ignores_garbled_concatenated_header() {
    // The reported bug: a concatenated `## Phase CICJ` header (4 letters, over
    // the cap) must be ignored — next-id derives from the real high-water `CA`,
    // giving `CB`, NOT `CICK` (the poisoned successor).
    let dir = scratch_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("PLAN.md"),
        "# PLAN\n## Phase BY - real\n## Phase CICJ - garbled\n## Phase CA - real\n",
    )
    .unwrap();
    assert_eq!(next_phase(&dir), "CB");
}

#[test]
fn marker_makes_next_phase_ignore_the_archive() {
    // With a marker present, PLAN_ARCHIVE.md is not consulted. Plant a
    // bogus-HIGH archived id (`ZZ`) that would dominate if scraped; next-id
    // must still come from the marker (`CI` -> `CJ`).
    let dir = scratch_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("PLAN.md"),
        "<!-- plan-bridge:phase-high-water=CI -->\n# PLAN\n## Phase B - live\n",
    )
    .unwrap();
    std::fs::write(dir.join("PLAN_ARCHIVE.md"), "## Phase ZZ - bogus\n").unwrap();
    assert_eq!(next_phase(&dir), "CJ");
}

#[test]
fn baseline_seeds_marker_then_next_phase_drops_archive_scrape() {
    // Migration path end-to-end: a pre-CJ plan (no marker) with a live phase
    // BELOW an already-archived id. Before baseline, next-id must read the
    // archive (markerless fallback) and clear it. After baseline seeds the
    // marker, next-id reads the marker and a later archive change is ignored.
    let dir = scratch_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("PLAN.md"),
        "# PLAN\n## Phase B - live\n- [ ] B.1 - open\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("PLAN_ARCHIVE.md"),
        "## Phase CA - swept\n## Phase CI - swept\n",
    )
    .unwrap();

    // Markerless: the archive is scraped, so next clears CI -> CJ.
    assert_eq!(next_phase(&dir), "CJ");

    // Seed the marker.
    let out = run(&["baseline"], &dir);
    assert!(
        out.status.success(),
        "baseline failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("phase high-water marker `CI`"),
        "baseline should report seeding CI: {stdout}"
    );
    let plan = std::fs::read_to_string(dir.join("PLAN.md")).unwrap();
    assert!(
        plan.starts_with("<!-- plan-bridge:phase-high-water=CI -->\n"),
        "marker not seeded at top:\n{plan}"
    );

    // Now mutate the archive to a bogus-high id. With the marker present it's
    // ignored — next-id stays CJ.
    std::fs::write(dir.join("PLAN_ARCHIVE.md"), "## Phase ZZ - bogus\n").unwrap();
    assert_eq!(next_phase(&dir), "CJ");
}

#[test]
fn archive_advances_marker_end_to_end() {
    // init a project, add a second phase, complete + archive the first, and
    // confirm the marker persisted the swept id so next-phase is correct even
    // though the swept header is gone from PLAN.md.
    let dir = scratch_dir();
    std::fs::create_dir_all(&dir).unwrap();
    // init ships Phase A + marker A.
    let out = run(&["init"], &dir);
    assert!(out.status.success(), "init failed");
    // Hand-add a live Phase B and complete Phase A's task so A can be swept.
    std::fs::write(
        dir.join("PLAN.md"),
        "<!-- plan-bridge:phase-high-water=A -->\n# PLAN\n## Phase A - first\n- [x] A.1 - done\n## Phase B - second\n- [ ] B.1 - open\n",
    )
    .unwrap();

    // Archive the fully-done Phase A.
    let out = run(&["archive", "A", "--date", "2026-07-11"], &dir);
    assert!(
        out.status.success(),
        "archive failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan = std::fs::read_to_string(dir.join("PLAN.md")).unwrap();
    assert!(!plan.contains("## Phase A - first"), "A should be swept");
    // Marker advanced to cover the highest id live at archive time (B).
    assert!(
        plan.contains("<!-- plan-bridge:phase-high-water=B -->"),
        "marker not advanced on archive:\n{plan}"
    );
    // next-phase = C (successor of the marker B), archive not needed.
    assert_eq!(next_phase(&dir), "C");
}

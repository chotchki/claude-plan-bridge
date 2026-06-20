//! Phase CA: end-to-end coverage for portable hook wiring + stale-cwd
//! diagnostics. Drives the built binary through the lifecycle a renamed or
//! freshly-cloned checkout hits: install → a stale baked `--cwd` creeps in →
//! `status` catches it → `upgrade-hooks` heals it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch_dir() -> PathBuf {
    plan_bridge::test_utils::scratch_dir("portable-hooks")
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_claude-plan-bridge"))
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

fn stale_settings(dead: &str) -> String {
    // A four-hook install whose every command bakes an absolute `--cwd`
    // pointing at `dead` — the shape a committed settings.json takes after the
    // repo is renamed or cloned to a different path.
    format!(
        r#"{{
  "hooks": {{
    "SessionStart": [{{"hooks": [{{"type": "command", "command": "claude-plan-bridge resume --cwd '{dead}'"}}]}}],
    "UserPromptSubmit": [{{"hooks": [{{"type": "command", "command": "claude-plan-bridge reconcile --cwd '{dead}'"}}]}}],
    "PostToolUse": [
      {{"matcher": "TaskCreate", "hooks": [{{"type": "command", "command": "claude-plan-bridge writeback --event create --cwd '{dead}'"}}]}},
      {{"matcher": "TaskUpdate", "hooks": [{{"type": "command", "command": "claude-plan-bridge writeback --event update --cwd '{dead}'"}}]}}
    ]
  }}
}}"#
    )
}

#[test]
fn portable_wiring_install_stale_diagnose_fix_lifecycle() {
    let dir = scratch_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let settings = dir.join(".claude/settings.json");

    // 1. `init` wires portable hooks — no machine-specific path on disk.
    let out = Command::new(binary())
        .args(["init", "--cwd"])
        .arg(&dir)
        .output()
        .expect("run init");
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let wired = read(&settings);
    assert!(
        wired.contains("$CLAUDE_PROJECT_DIR"),
        "init did not write portable hooks: {wired}"
    );
    assert!(
        !wired.contains(&dir.to_string_lossy().to_string()),
        "init baked the machine-specific project path into the hooks: {wired}"
    );

    // 2. Simulate a stale checkout: rewrite settings to bake a dead absolute
    //    `--cwd` (a directory that does not exist). PLAN.md is present and fine
    //    — the breakage is purely the misrouted hook cwd.
    let ghost = dir.join("renamed-away");
    let ghost_s = ghost.to_string_lossy().to_string();
    assert!(!ghost.exists(), "ghost dir must not exist");
    std::fs::write(&settings, stale_settings(&ghost_s)).unwrap();
    std::fs::write(dir.join("PLAN.md"), "# PLAN\n- [ ] 1.0 Phase\n").unwrap();

    // 3. `status` flags the stale path loudly and points at the fix.
    let out = Command::new(binary())
        .args(["status", "--cwd"])
        .arg(&dir)
        .output()
        .expect("run status");
    let status_text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        status_text.contains("no longer exists") && status_text.contains(&ghost_s),
        "status did not flag the stale baked --cwd: {status_text}"
    );
    assert!(
        status_text.contains("upgrade-hooks"),
        "status did not point at the fix: {status_text}"
    );

    // 4. `upgrade-hooks` rewrites every command to the portable form and drops
    //    the stale path.
    let out = Command::new(binary())
        .args(["upgrade-hooks", "--cwd"])
        .arg(&dir)
        .output()
        .expect("run upgrade-hooks");
    assert!(
        out.status.success(),
        "upgrade-hooks failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let healed = read(&settings);
    assert!(
        healed.contains("$CLAUDE_PROJECT_DIR"),
        "upgrade-hooks did not write portable hooks: {healed}"
    );
    assert!(
        !healed.contains(&ghost_s),
        "stale path survived upgrade-hooks: {healed}"
    );

    // 5. `status` is quiet about stale wiring now.
    let out = Command::new(binary())
        .args(["status", "--cwd"])
        .arg(&dir)
        .output()
        .expect("run status again");
    let status_text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !status_text.contains("no longer exists"),
        "status still reports a stale cwd after the fix: {status_text}"
    );
}

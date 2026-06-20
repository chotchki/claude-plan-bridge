//! Phase BZ: end-to-end coverage of the phase-naming flow through the real
//! MCP server over stdio. Spawns `claude-plan-bridge serve`, asks for the next
//! phase id, auto-assigns a phase with no `id`, and verifies the on-disk plan —
//! exercising the scan (live + archive) -> next id -> auto-assign chain across
//! the process boundary.

use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn scratch_dir() -> PathBuf {
    plan_bridge::test_utils::scratch_dir("next-phase-mcp")
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_claude-plan-bridge"))
}

#[test]
fn next_phase_mcp_e2e_auto_assigns_clearing_archive() {
    let dir = scratch_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let plan = dir.join("PLAN.md");
    // Live high-water mark BY; CA already swept to the archive. The next id
    // must clear BOTH -> CB.
    std::fs::write(&plan, "## Phase BY - Prior\n\n- [ ] BY.1 - a task\n").unwrap();
    std::fs::write(dir.join("PLAN_ARCHIVE.md"), "## Phase CA - swept\n").unwrap();

    let mut child = Command::new(binary())
        .args(["serve", "--cwd"])
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");

    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "plan_next_phase", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "plan_add_phase", "arguments": {"title": "Auto"}}}),
    ];
    {
        let mut stdin = child.stdin.take().unwrap();
        for r in &requests {
            writeln!(stdin, "{}", serde_json::to_string(r).unwrap()).unwrap();
        }
        // stdin dropped here -> EOF -> the serve read loop ends and exits.
    }

    let out = child.wait_with_output().expect("wait serve");
    assert!(
        out.status.success(),
        "serve exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // The read-only plan_next_phase (id 2) reports CB before anything mutates.
    let next_text = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["id"] == json!(2))
        .map(|v| {
            v["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .expect("a response for id 2");
    assert_eq!(
        next_text, "CB",
        "plan_next_phase should report CB: {stdout}"
    );

    // plan_add_phase with no id auto-assigned CB and wrote the header.
    let after = std::fs::read_to_string(&plan).unwrap();
    assert!(
        after.contains("## Phase CB - Auto"),
        "auto-assigned phase landed:\n{after}"
    );
    assert!(
        !after.contains("## Phase CA"),
        "must not reuse the archived CA:\n{after}"
    );
}

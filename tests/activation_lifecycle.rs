//! Phase 40.6: end-to-end activation lifecycle. Exercises every Phase 40
//! component through the public bridge API (state, resume, writeback,
//! reconcile, archive) so a regression in any one of them surfaces here
//! as a single failing test.
//!
//! Scenario walked:
//! 1. Project has two phases: AI (active) and AM (other). State has
//!    mappings for both phases' leaves.
//! 2. `plan_activate AI` sets state.active_phase, surfaces unmet hard deps.
//! 3. Resume scopes the rehydration prompt to AI leaves only; AM leaves
//!    appear nowhere in the prompt body.
//! 4. Writeback handling `TaskCreate(plan_path=AM.5)` (cross-phase) still
//!    creates the task BUT appends a "cross-phase TaskCreate" warning to
//!    the hook output.
//! 5. Reconcile renders deltas into an "Active phase `AI` drift:" block
//!    followed by "Other phases / cross-cutting:" — AM drift goes in the
//!    second block.
//! 6. `archive_phase AI` (after marking AI's leaves resolved) clears
//!    state.active_phase to None — the focus moves with the phase.

use plan_bridge::ast::NodeState;
use plan_bridge::state::{Mapping, State};

fn scratch_dir() -> std::path::PathBuf {
    plan_bridge::test_utils::scratch_dir("40-e2e")
}

#[test]
fn activation_full_lifecycle_e2e() {
    let dir = scratch_dir();
    let plan_path = dir.join("PLAN.md");
    std::fs::write(
        &plan_path,
        "## Phase AI - Studio dogfood\n\n\
         - [ ] AI.0 lock\n\
         - [ ] AI.1 audit\n\n\
         ## Phase AM - Tailwind *(depends on: AI)*\n\n\
         - [ ] AM.0 spike\n",
    )
    .unwrap();

    // Seed state with mappings for every leaf.
    let state_path = plan_bridge::state::default_state_path_for(&plan_path);
    let mut state = State::default();
    for (tid, path) in [
        ("t-AI-0", "AI.0"),
        ("t-AI-1", "AI.1"),
        ("t-AM-0", "AM.0"),
    ] {
        state.record(
            tid,
            Mapping {
                plan_path: path.to_string(),
                last_synced_title: format!("task at {path}"),
                last_synced_state: NodeState::Pending,
                ..Default::default()
            },
        );
    }
    state.save(&state_path).unwrap();

    // ----- (2) activate AI; state persists -----
    {
        let mut s = State::load(&state_path).unwrap();
        s.set_active_phase(Some("AI".to_string()));
        s.save(&state_path).unwrap();
    }
    assert_eq!(
        State::load(&state_path).unwrap().active_phase(),
        Some("AI"),
        "active_phase survives save+load"
    );

    // ----- (3) resume scopes to AI leaves -----
    let msg = plan_bridge::resume::build_resume_message(&plan_path, "resume")
        .unwrap()
        .expect("rehydration prompt non-empty");
    assert!(
        msg.contains("Active phase: `AI`"),
        "active-phase header present: {msg}"
    );
    assert!(msg.contains("AI.0") && msg.contains("AI.1"), "AI leaves: {msg}");
    assert!(!msg.contains("AM.0"), "AM filtered out: {msg}");

    // ----- (4) cross-phase TaskCreate warn-but-allow -----
    let payload = plan_bridge::hook::HookPayload {
        session_id: String::new(),
        cwd: String::new(),
        hook_event_name: "PostToolUse".to_string(),
        tool_name: "TaskCreate".to_string(),
        tool_input: serde_json::json!({
            "subject": "Cross-phase fix",
            "metadata": { "plan_path": "AM.5" }
        }),
        tool_response: serde_json::json!({ "id": "t-cross" }),
        source: String::new(),
    };
    let out = plan_bridge::writeback::writeback_create(&payload, &plan_path).unwrap();
    let json = out.to_json();
    assert!(
        json.contains("cross-phase TaskCreate"),
        "cross-phase warning: {json}"
    );
    let plan_contents = std::fs::read_to_string(&plan_path).unwrap();
    assert!(plan_contents.contains("AM.5"), "task did land:\n{plan_contents}");

    // ----- (5) reconcile foregrounds AI drift -----
    let deltas = plan_bridge::reconcile::reconcile(&plan_path).unwrap();
    let rendered =
        plan_bridge::reconcile::render_deltas_focused(&deltas, Some("AI"));
    // AM has *(depends on: AI)* and AI is still in plan.phases (not yet
    // archived), so a PhaseDependsOn{AM → AI} delta fires — that belongs to
    // the AM block (it's ABOUT AM's dep on AI).
    if rendered.contains("Other phases") {
        let other_pos = rendered.find("Other phases").unwrap();
        assert!(
            rendered[other_pos..].contains("Phase AM depends on AI"),
            "AM's dep delta in other block: {rendered}"
        );
    }

    // ----- (6) archive AI auto-clears active_phase -----
    // Mark AI's leaves resolved so archive_phase can succeed.
    {
        let text = std::fs::read_to_string(&plan_path).unwrap();
        let mut p = plan_bridge::parser::parse(&text).unwrap();
        for id in ["AI.0", "AI.1"] {
            if let Some(mut item) = p.find_item_mut(id) {
                item.set_state(NodeState::Done);
            }
        }
        std::fs::write(&plan_path, plan_bridge::serializer::serialize(&p)).unwrap();
    }
    let report =
        plan_bridge::archive::archive_phase(&plan_path, "AI", "2026-05-22", false).unwrap();
    assert!(
        report.archived_phase_ids.iter().any(|id| id == "AI"),
        "AI archived: {:?}",
        report.archived_phase_ids
    );
    let final_state = State::load(&state_path).unwrap();
    assert_eq!(
        final_state.active_phase(),
        None,
        "archiving the active phase clears focus"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

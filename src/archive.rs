#[cfg(test)]
use crate::ast::NodeState;
use crate::ast::{Node, Phase, Plan};
use crate::parser::parse;
use crate::serializer::serialize;
use crate::state::{State, default_state_path_for};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Outcome of an archive sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveReport {
    pub archived_phase_ids: Vec<String>,
    pub archived_plan_paths: Vec<String>,
    pub dry_run: bool,
}

impl ArchiveReport {
    pub fn empty(dry_run: bool) -> Self {
        Self {
            archived_phase_ids: vec![],
            archived_plan_paths: vec![],
            dry_run,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.archived_phase_ids.is_empty()
    }
}

/// Sweep every fully-complete top-level phase from PLAN.md into PLAN_ARCHIVE.md.
///
/// A phase is "fully complete" when every leaf in its subtree is `[x]`. The
/// phase's own checkbox state is irrelevant — parents auto-tick semantically;
/// the bridge doesn't write that through. Stable ids are preserved (no
/// renumbering — see `plan-id-stability` memory).
///
/// With `dry_run=true`, returns the list of phases that *would* be archived
/// without touching the filesystem.
pub fn archive(plan_path: &Path, dry_run: bool, today: &str) -> Result<ArchiveReport> {
    let text = std::fs::read_to_string(plan_path)
        .with_context(|| format!("read {}", plan_path.display()))?;
    let parsed = parse(&text).with_context(|| format!("parse {}", plan_path.display()))?;
    let (mut plan, _notes) = parsed
        .standardize_to_canonical()
        .map_err(anyhow::Error::msg)?;

    // Partition phases into "stay" vs "archive" preserving order.
    let mut keep: Vec<Phase> = Vec::new();
    let mut archive: Vec<Phase> = Vec::new();
    for phase in std::mem::take(&mut plan.phases) {
        if phase_fully_done(&phase) {
            archive.push(phase);
        } else {
            keep.push(phase);
        }
    }

    let mut report = ArchiveReport::empty(dry_run);
    for phase in &archive {
        report.archived_phase_ids.push(phase.id.clone());
        collect_phase_paths(phase, &mut report.archived_plan_paths);
    }

    if archive.is_empty() {
        return Ok(report);
    }

    if dry_run {
        return Ok(report);
    }

    // Build the archive section content.
    plan.phases = keep;
    let new_plan_text = serialize(&plan);

    let archive_section = build_archive_section(today, &archive);
    let archive_path = archive_path_for(plan_path);
    let archive_text = if archive_path.exists() {
        std::fs::read_to_string(&archive_path)
            .with_context(|| format!("read {}", archive_path.display()))?
    } else {
        String::new()
    };
    let combined = append_archive(&archive_text, &archive_section);

    atomic_write(plan_path, new_plan_text.as_bytes())
        .with_context(|| format!("write {}", plan_path.display()))?;
    atomic_write(&archive_path, combined.as_bytes())
        .with_context(|| format!("write {}", archive_path.display()))?;

    // Drop state mappings whose plan_path lives inside any archived subtree.
    let state_path = default_state_path_for(plan_path);
    let mut state = State::load(&state_path)?;
    let archived: std::collections::HashSet<&str> = report
        .archived_plan_paths
        .iter()
        .map(String::as_str)
        .collect();
    let to_drop: Vec<String> = state
        .mappings
        .iter()
        .filter(|(_, m)| archived.contains(m.plan_path.as_str()))
        .map(|(tid, _)| tid.clone())
        .collect();
    for tid in &to_drop {
        state.remove(tid);
    }
    // Phase 40.4: if the active phase was among the archived ids, clear it
    // — focus on a vanished phase is nonsense, and the next resume would
    // emit an empty rehydration prompt.
    let active_cleared = match state.active_phase() {
        Some(active) if report.archived_phase_ids.iter().any(|id| id == active) => {
            state.set_active_phase(None);
            true
        }
        _ => false,
    };
    if !to_drop.is_empty() || active_cleared {
        state.save(&state_path)?;
    }

    Ok(report)
}

/// Archive a single phase by id. Validates the subtree is fully resolved
/// (`[x]` or `[-]` leaves) — errors otherwise. The phase is moved to the same
/// dated section as `archive` would write; state mappings whose `plan_path`
/// lives inside the moved subtree are dropped.
///
/// When `descope_pending` is true, pending leaves are moved to the bottom
/// `# Backlog (not yet phased)` section before the archive proceeds — the
/// 38.5 "descope-and-archive" escape hatch. Each descoped leaf lands as a
/// bullet `- <id> - <title>` in plan.backlog, and is removed from the phase
/// subtree. State mappings for descoped paths are dropped (they're no longer
/// tracked tasks; they're notes in the backlog).
pub fn archive_phase(
    plan_path: &Path,
    phase_id: &str,
    today: &str,
    descope_pending: bool,
) -> Result<ArchiveReport> {
    let text = std::fs::read_to_string(plan_path)
        .with_context(|| format!("read {}", plan_path.display()))?;
    let parsed = parse(&text).with_context(|| format!("parse {}", plan_path.display()))?;
    let (mut plan, _notes) = parsed
        .standardize_to_canonical()
        .map_err(anyhow::Error::msg)?;

    let phase_idx = plan
        .phases
        .iter()
        .position(|p| p.id == phase_id)
        .ok_or_else(|| anyhow::anyhow!("no phase with id `{phase_id}` at top level"))?;

    let descoped_paths: Vec<String> = if !phase_fully_done(&plan.phases[phase_idx]) {
        if descope_pending {
            let descoped = descope_pending_leaves(&mut plan.phases[phase_idx]);
            for path in &descoped {
                plan.backlog
                    .push(format!("- {path} - descoped from phase `{phase_id}` on {today}"));
            }
            // Backlog flipped to h1 alongside descope — descoping IS a v2
            // operation, so the bottom section adopts the FORMATv2 heading.
            plan.backlog_h1 = true;
            if !phase_fully_done(&plan.phases[phase_idx]) {
                let mut still_unresolved: Vec<String> = Vec::new();
                collect_unresolved_leaves_in_phase(
                    &plan.phases[phase_idx],
                    &mut still_unresolved,
                );
                anyhow::bail!(
                    "phase `{phase_id}` still has unresolved leaves after --descope-pending: {} \
                     (these are non-leaf nodes whose own state is pending; tick or `[-]` them first)",
                    still_unresolved.join(", ")
                );
            }
            descoped
        } else {
            let mut unresolved: Vec<String> = Vec::new();
            collect_unresolved_leaves_in_phase(&plan.phases[phase_idx], &mut unresolved);
            anyhow::bail!(
                "phase `{phase_id}` is not fully resolved; unresolved leaves: {} \
                 (re-run with --descope-pending to move pending leaves to the # Backlog section first)",
                unresolved.join(", ")
            );
        }
    } else {
        Vec::new()
    };

    let phase = plan.phases.remove(phase_idx);
    let mut report = ArchiveReport::empty(false);
    report.archived_phase_ids.push(phase.id.clone());
    collect_phase_paths(&phase, &mut report.archived_plan_paths);

    let new_plan_text = serialize(&plan);
    let archive_section = build_archive_section(today, std::slice::from_ref(&phase));
    let archive_path = archive_path_for(plan_path);
    let archive_text = if archive_path.exists() {
        std::fs::read_to_string(&archive_path)?
    } else {
        String::new()
    };
    let combined = append_archive(&archive_text, &archive_section);

    atomic_write(plan_path, new_plan_text.as_bytes())?;
    atomic_write(&archive_path, combined.as_bytes())?;

    let state_path = crate::state::default_state_path_for(plan_path);
    let mut state = crate::state::State::load(&state_path)?;
    // Drop mappings for both archived AND descoped paths — descoped tasks
    // are no longer tracked; they're notes in the backlog.
    let removed: std::collections::HashSet<&str> = report
        .archived_plan_paths
        .iter()
        .chain(descoped_paths.iter())
        .map(String::as_str)
        .collect();
    let to_drop: Vec<String> = state
        .mappings
        .iter()
        .filter(|(_, m)| removed.contains(m.plan_path.as_str()))
        .map(|(tid, _)| tid.clone())
        .collect();
    for tid in &to_drop {
        state.remove(tid);
    }
    // Phase 40.4: clear active_phase when archiving the focused phase.
    let active_cleared = match state.active_phase() {
        Some(active) if active == phase_id => {
            state.set_active_phase(None);
            true
        }
        _ => false,
    };
    if !to_drop.is_empty() || active_cleared {
        state.save(&state_path)?;
    }

    Ok(report)
}

fn collect_unresolved_leaves(node: &Node, out: &mut Vec<String>) {
    if node.is_leaf() {
        if !node.is_resolved() {
            out.push(node.id.clone());
        }
        return;
    }
    for child in &node.children {
        collect_unresolved_leaves(child, out);
    }
}

/// Phase-level entry point for unresolved-leaf collection. Phases themselves
/// aren't leaves — recurse straight into their task list.
fn collect_unresolved_leaves_in_phase(phase: &Phase, out: &mut Vec<String>) {
    for child in &phase.children {
        collect_unresolved_leaves(child, out);
    }
}

/// Walk a phase's task subtree and remove every pending leaf, returning the
/// removed leaves' plan paths in document order. Used by 38.5's
/// `--descope-pending` archive escape hatch — each removed leaf becomes a
/// backlog note before the phase archive proceeds.
///
/// Leaf-only: a non-leaf node with pending children is left in place (its
/// children are handled individually). After the pass an intermediate parent
/// that had only-pending leaves may become childless — those parents stay as
/// structural nodes that go to PLAN_ARCHIVE.md with the rest of the phase.
fn descope_pending_leaves(phase: &mut Phase) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < phase.children.len() {
        if phase.children[i].is_leaf()
            && phase.children[i].state == crate::ast::NodeState::Pending
        {
            out.push(phase.children[i].id.clone());
            phase.children.remove(i);
        } else {
            descope_pending_in_node(&mut phase.children[i], &mut out);
            i += 1;
        }
    }
    out
}

fn descope_pending_in_node(node: &mut Node, out: &mut Vec<String>) {
    let mut i = 0;
    while i < node.children.len() {
        if node.children[i].is_leaf() && node.children[i].state == crate::ast::NodeState::Pending {
            out.push(node.children[i].id.clone());
            node.children.remove(i);
        } else {
            descope_pending_in_node(&mut node.children[i], out);
            i += 1;
        }
    }
}

fn node_fully_done(node: &Node) -> bool {
    if node.is_leaf() {
        return node.is_resolved();
    }
    node.children.iter().all(node_fully_done)
}

/// True when every leaf under the phase is resolved. For non-empty phases
/// the phase's own (legacy) state checkbox is irrelevant — phases auto-tick
/// semantically. For an *empty* phase (no children) we fall back to the
/// legacy state: `[ ]` blocks archive, `[x]`/`[-]`/`[>]` clears it.
fn phase_fully_done(phase: &Phase) -> bool {
    if phase.children.is_empty() {
        return phase.is_resolved();
    }
    phase.children.iter().all(node_fully_done)
}

fn collect_plan_paths(node: &Node, out: &mut Vec<String>) {
    out.push(node.id.clone());
    for child in &node.children {
        collect_plan_paths(child, out);
    }
}

fn collect_phase_paths(phase: &Phase, out: &mut Vec<String>) {
    out.push(phase.id.clone());
    for child in &phase.children {
        collect_plan_paths(child, out);
    }
}

fn build_archive_section(today: &str, archived: &[Phase]) -> String {
    let mut out = format!("## {today}\n\n");
    for phase in archived {
        let temp = Plan {
            preamble: vec![],
            phases: vec![phase.clone()],
            backlog: vec![],
            backlog_h1: false,
        };
        out.push_str(&serialize(&temp));
        out.push('\n');
    }
    out
}

/// Append `new_section` to the end of `existing` archive content, separated by
/// a `---` divider. History reads chronological-ascending: oldest sweep at the
/// top, newest at the bottom.
fn append_archive(existing: &str, new_section: &str) -> String {
    if existing.is_empty() {
        return new_section.to_string();
    }
    let mut combined = existing.to_string();
    if !combined.ends_with("\n\n") {
        if combined.ends_with('\n') {
            combined.push('\n');
        } else {
            combined.push_str("\n\n");
        }
    }
    combined.push_str("---\n\n");
    combined.push_str(new_section);
    combined
}

fn archive_path_for(plan_path: &Path) -> PathBuf {
    plan_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("PLAN_ARCHIVE.md")
}

// Phase 41.4: atomic_write moved to crate::io_util.
use crate::io_util::atomic_write;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Mapping, State, default_state_path_for};
    use crate::test_utils::write_plan;

    // Phase 41.2: scratch_dir() / uniq() / write_plan() moved to
    // crate::test_utils. scratch_dir takes a per-module prefix string.
    fn scratch_dir() -> std::path::PathBuf {
        crate::test_utils::scratch_dir("archive")
    }

    #[test]
    fn no_op_when_nothing_complete() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        let report = archive(&plan, false, "2026-05-16").unwrap();
        assert!(report.is_empty());
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(after.contains("1.0 Phase"));
        assert!(!archive_path_for(&plan).exists());
    }

    #[test]
    fn prefix_bundle_reports_one_phase_many_items() {
        // The under-report case the summary message guards against: a single
        // top-level phase `AE.0` bundling `AE.1`..`AE.3` is 1 phase but 4 items.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "\
- [ ] AE.0 Phase AE
  - [x] AE.1 One
  - [x] AE.2 Two
  - [-] AE.3 Three (won't do)
",
        );
        let report = archive(&plan, false, "2026-05-19").unwrap();
        assert_eq!(
            report.archived_phase_ids,
            vec!["AE.0"],
            "one top-level phase"
        );
        assert_eq!(
            report.archived_plan_paths.len(),
            4,
            "four items archived: {:?}",
            report.archived_plan_paths
        );
    }

    #[test]
    fn archives_fully_complete_phase() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "\
- [ ] 1.0 Done phase
  - [x] 1.1 Done
  - [x] 1.2 Also done
- [ ] 2.0 Still going
  - [ ] 2.1 Pending
",
        );
        let report = archive(&plan, false, "2026-05-16").unwrap();
        assert_eq!(report.archived_phase_ids, vec!["1.0"]);
        assert!(report.archived_plan_paths.contains(&"1.0".to_string()));
        assert!(report.archived_plan_paths.contains(&"1.1".to_string()));
        assert!(report.archived_plan_paths.contains(&"1.2".to_string()));

        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(!after.contains("1.0 Done phase"));
        assert!(after.contains("2.0 Still going"));

        let archive_text = std::fs::read_to_string(archive_path_for(&plan)).unwrap();
        assert!(archive_text.starts_with("## 2026-05-16"));
        assert!(archive_text.contains("1.0 Done phase"));
        assert!(archive_text.contains("1.1 Done"));
    }

    #[test]
    fn backlog_leaves_count_as_resolved_for_archive() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "\
- [ ] 1.0 Mixed-resolved phase
  - [x] 1.1 Done
  - [-] 1.2 Skipped
  - [>] 1.3 Deferred
",
        );
        let report = archive(&plan, false, "2026-05-17").unwrap();
        assert_eq!(report.archived_phase_ids, vec!["1.0"]);
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(!after.contains("1.0 Mixed-resolved phase"));
        let archive_text = std::fs::read_to_string(archive_path_for(&plan)).unwrap();
        assert!(archive_text.contains("- [>] 1.3 Deferred"));
    }

    #[test]
    fn parent_unchecked_but_children_all_done_still_archives() {
        // Bridge doesn't auto-tick parents; a phase whose box reads `[ ]` but
        // whose subtree is fully `[x]` should archive based on subtree state.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Parent unchecked\n  - [x] 1.1 Done\n");
        let report = archive(&plan, false, "2026-05-16").unwrap();
        assert_eq!(report.archived_phase_ids, vec!["1.0"]);
    }

    #[test]
    fn appends_to_existing_archive() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [x] 1.1 Done\n");
        let archive_path = archive_path_for(&plan);
        std::fs::write(&archive_path, "## 2026-04-01\n\n- [x] 0.0 Earlier work\n").unwrap();
        archive(&plan, false, "2026-05-16").unwrap();
        let archive_text = std::fs::read_to_string(&archive_path).unwrap();
        let pos_new = archive_text
            .find("## 2026-05-16")
            .expect("new section present");
        let pos_old = archive_text
            .find("## 2026-04-01")
            .expect("old section preserved");
        assert!(pos_old < pos_new, "newest should be appended at the bottom");
        assert!(archive_text.contains("0.0 Earlier work"));
        assert!(archive_text.contains("---"), "divider between sections");
    }

    #[test]
    fn append_preserves_multiple_existing_sections_in_order() {
        // Regression for Phase 7 ordering: when PLAN_ARCHIVE.md already has
        // two dated sections, a new sweep appends *after* both — the original
        // section order is preserved.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 9.0 Phase\n  - [x] 9.1 Done\n");
        let archive_path = archive_path_for(&plan);
        std::fs::write(
            &archive_path,
            "## 2026-01-01\n\n- [x] 1.0 First\n\n---\n\n## 2026-03-01\n\n- [x] 2.0 Second\n",
        )
        .unwrap();
        archive(&plan, false, "2026-05-16").unwrap();
        let archive_text = std::fs::read_to_string(&archive_path).unwrap();
        let p1 = archive_text
            .find("## 2026-01-01")
            .expect("section 1 preserved");
        let p2 = archive_text
            .find("## 2026-03-01")
            .expect("section 2 preserved");
        let p3 = archive_text
            .find("## 2026-05-16")
            .expect("new section present");
        assert!(
            p1 < p2 && p2 < p3,
            "sections must read chronological-ascending; got order {p1},{p2},{p3}"
        );
    }

    #[test]
    fn dry_run_does_not_mutate() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [x] 1.1 Done\n");
        let before = std::fs::read_to_string(&plan).unwrap();
        let report = archive(&plan, true, "2026-05-16").unwrap();
        assert!(report.dry_run);
        assert_eq!(report.archived_phase_ids, vec!["1.0"]);
        let after = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(before, after);
        assert!(!archive_path_for(&plan).exists());
    }

    #[test]
    fn drops_state_mappings_for_archived_nodes() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [x] 1.1 Done\n");
        let state_path = default_state_path_for(&plan);
        let mut state = State::default();
        state.record(
            "t-archived",
            Mapping {
                plan_path: "1.1".to_string(),
                last_synced_title: "Done".to_string(),
                last_synced_state: NodeState::Done,
                last_synced_annotations: vec![],
                ..Default::default()
            },
        );
        state.record(
            "t-elsewhere",
            Mapping {
                plan_path: "9.9".to_string(),
                last_synced_title: "x".to_string(),
                last_synced_state: NodeState::Pending,
                last_synced_annotations: vec![],
                ..Default::default()
            },
        );
        state.save(&state_path).unwrap();

        archive(&plan, false, "2026-05-16").unwrap();
        let loaded = State::load(&state_path).unwrap();
        assert_eq!(
            loaded.plan_path("t-archived"),
            None,
            "archived mapping should be gone"
        );
        assert_eq!(
            loaded.plan_path("t-elsewhere"),
            Some("9.9"),
            "unrelated mapping should remain"
        );
    }

    #[test]
    fn empty_leaf_phase_unchecked_does_not_archive() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Empty phase\n");
        let report = archive(&plan, false, "2026-05-16").unwrap();
        assert!(report.is_empty());
    }

    #[test]
    fn empty_leaf_phase_checked_archives() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [x] 1.0 Empty phase\n");
        let report = archive(&plan, false, "2026-05-16").unwrap();
        assert_eq!(report.archived_phase_ids, vec!["1.0"]);
    }

    #[test]
    fn phase_with_all_wont_do_leaves_archives() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [-] 1.1 Skipped\n  - [-] 1.2 Also skipped\n",
        );
        let report = archive(&plan, false, "2026-05-16").unwrap();
        assert_eq!(report.archived_phase_ids, vec!["1.0"]);
    }

    #[test]
    fn phase_with_mix_of_done_and_wont_do_archives() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [x] 1.1 Done\n  - [-] 1.2 Skipped\n",
        );
        let report = archive(&plan, false, "2026-05-16").unwrap();
        assert_eq!(report.archived_phase_ids, vec!["1.0"]);
    }

    #[test]
    fn archive_phase_targets_a_specific_phase() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase one\n  - [x] 1.1 Done\n- [ ] 2.0 Phase two\n  - [x] 2.1 Also done\n",
        );
        // Even though both phases are fully done, archive_phase only moves 1.0.
        let report = archive_phase(&plan, "1.0", "2026-05-16", false).unwrap();
        assert_eq!(report.archived_phase_ids, vec!["1.0"]);
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(!after.contains("1.0 Phase one"));
        assert!(after.contains("2.0 Phase two"));
    }

    #[test]
    fn archive_phase_refuses_unresolved_subtree() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Not done\n");
        let err = archive_phase(&plan, "1.0", "2026-05-16", false).unwrap_err();
        assert!(err.to_string().contains("not fully resolved"), "{err}");
    }

    #[test]
    fn archive_phase_errors_when_phase_missing() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n");
        let err = archive_phase(&plan, "9.9", "2026-05-16", false).unwrap_err();
        assert!(err.to_string().contains("9.9"));
    }

    #[test]
    fn archive_phase_error_message_mentions_descope_escape_hatch() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [x] 1.1 done\n  - [ ] 1.2 pending\n",
        );
        let err = archive_phase(&plan, "1.0", "2026-05-22", false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--descope-pending"), "error mentions the flag: {msg}");
        assert!(msg.contains("1.2"), "error lists the unresolved leaf: {msg}");
    }

    #[test]
    fn archive_phase_with_descope_pending_moves_leaves_to_backlog_and_archives() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [x] 1.1 done\n  - [ ] 1.2 still pending\n  - [ ] 1.3 also pending\n",
        );
        let report = archive_phase(&plan, "1.0", "2026-05-22", true).unwrap();
        assert_eq!(report.archived_phase_ids, vec!["1.0"]);

        // PLAN.md: phase 1.0 is gone, backlog has the descoped notes (h1).
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(!after.contains("1.0 Phase"), "phase archived:\n{after}");
        assert!(after.contains("# Backlog (not yet phased)"));
        assert!(after.contains("- 1.2 - descoped from phase `1.0` on 2026-05-22"));
        assert!(after.contains("- 1.3 - descoped from phase `1.0` on 2026-05-22"));

        // PLAN_ARCHIVE.md got the phase with its single done task.
        let archive_md = std::fs::read_to_string(&dir.join("PLAN_ARCHIVE.md")).unwrap();
        assert!(archive_md.contains("1.0 Phase"));
        assert!(archive_md.contains("1.1 done"));
        // Descoped leaves were removed before archive, so they're NOT in
        // PLAN_ARCHIVE.md.
        assert!(!archive_md.contains("1.2 still pending"));
        assert!(!archive_md.contains("1.3 also pending"));
    }

    #[test]
    fn archive_phase_descope_is_noop_when_already_fully_resolved() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [x] 1.1 done\n  - [-] 1.2 won't-do\n");
        // descope_pending=true on a fully-resolved phase: no leaves to
        // descope, no backlog notes added, phase archives normally.
        let report = archive_phase(&plan, "1.0", "2026-05-22", true).unwrap();
        assert_eq!(report.archived_phase_ids, vec!["1.0"]);
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(!after.contains("Backlog"), "no backlog noise:\n{after}");
    }

    // -----------------------------------------------------------------
    // Phase 40.4: archive auto-clears active_phase when archiving the
    // focused phase
    // -----------------------------------------------------------------

    #[test]
    fn archive_phase_clears_state_active_phase_when_it_targets_the_focused_phase() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [x] 1.1 done\n  - [x] 1.2 also done\n",
        );
        let state_path = default_state_path_for(&plan);
        let mut state = State::default();
        state.set_active_phase(Some("1.0".to_string()));
        state.save(&state_path).unwrap();

        let _ = archive_phase(&plan, "1.0", "2026-05-22", false).unwrap();
        let after_state = State::load(&state_path).unwrap();
        assert_eq!(
            after_state.active_phase(),
            None,
            "archived phase cleared active_phase"
        );
    }

    #[test]
    fn archive_phase_preserves_state_active_phase_when_different() {
        // Active phase AS, archive 1.0 — focus stays on AS.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [x] 1.1 done\n\n## Phase AS - Spine\n\n- [ ] AS.0 task\n",
        );
        let state_path = default_state_path_for(&plan);
        let mut state = State::default();
        state.set_active_phase(Some("AS".to_string()));
        state.save(&state_path).unwrap();

        let _ = archive_phase(&plan, "1.0", "2026-05-22", false).unwrap();
        let after_state = State::load(&state_path).unwrap();
        assert_eq!(after_state.active_phase(), Some("AS"));
    }

    #[test]
    fn bulk_archive_clears_state_active_phase_when_focused_phase_swept() {
        // Active phase is one of the fully-complete phases the bulk sweep
        // picks up — clear the focus.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 First\n  - [x] 1.1 done\n- [ ] 2.0 Second\n  - [ ] 2.1 pending\n",
        );
        let state_path = default_state_path_for(&plan);
        let mut state = State::default();
        state.set_active_phase(Some("1.0".to_string()));
        state.save(&state_path).unwrap();

        let report = archive(&plan, false, "2026-05-22").unwrap();
        assert_eq!(report.archived_phase_ids, vec!["1.0"]);
        let after_state = State::load(&state_path).unwrap();
        assert_eq!(
            after_state.active_phase(),
            None,
            "swept active phase cleared focus"
        );
    }

    #[test]
    fn bulk_archive_preserves_state_active_phase_when_not_swept() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 First\n  - [x] 1.1 done\n- [ ] 2.0 Second\n  - [ ] 2.1 pending\n",
        );
        let state_path = default_state_path_for(&plan);
        let mut state = State::default();
        // Active phase is 2.0, which has pending leaves → bulk sweep skips it.
        state.set_active_phase(Some("2.0".to_string()));
        state.save(&state_path).unwrap();

        let _ = archive(&plan, false, "2026-05-22").unwrap();
        let after_state = State::load(&state_path).unwrap();
        assert_eq!(after_state.active_phase(), Some("2.0"));
    }
}

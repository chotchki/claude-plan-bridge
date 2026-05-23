use anyhow::{Context, Result};
use std::path::Path;

use crate::ast::{Annotation, NodeState, PhaseSource, Separator};
use crate::parser::parse;
use crate::serializer::serialize;

pub struct CanonicalizeReport {
    pub notes: Vec<String>,
    pub changed: bool,
    pub dry_run: bool,
}

/// Run the canonical-form pass against PLAN.md explicitly. Promotes
/// `### Phase N — Title` markdown headers into proper `N.0` phase checkboxes,
/// strips bold-wrapped IDs (`**X.4.a.1**` → `X.4.a.1`), normalizes em-dash /
/// hyphen separators to plain space, and rewrites the file with serialized
/// canonical form.
///
/// Before Phase 29 this ran implicitly on every writeback / MCP plan_* call.
/// After Phase 29 it's opt-in — adopters with bespoke format conventions
/// (`### subsections`, bold IDs, em-dash separators) keep their format on
/// routine writes and only see the canonical mow when they explicitly invoke
/// this subcommand.
///
/// `dry_run` parses + standardizes but doesn't write the file. The notes are
/// still returned so the caller can preview promotions before committing.
pub fn canonicalize(plan_path: &Path, dry_run: bool) -> Result<CanonicalizeReport> {
    let text = std::fs::read_to_string(plan_path)
        .with_context(|| format!("read {}", plan_path.display()))?;
    let parsed = parse(&text)?;
    let original_serialized = serialize(&parsed);
    let (mut plan, mut notes) = parsed
        .standardize_to_canonical()
        .map_err(anyhow::Error::msg)?;
    // Phase 35.6a: sweep any scattered/duplicate `## Backlog (not yet phased)`
    // (in the preamble or dangling as annotations) into the single canonical
    // bottom section. This is the one place that relocates an existing Backlog
    // — routine writes leave it where it sits.
    let swept = plan.consolidate_backlog();
    if swept > 0 {
        notes.push(format!(
            "consolidated {swept} backlog item(s) into the bottom Backlog section"
        ));
    }

    // Phase 37.5: v1 → v2 format flip.
    //   - Every Phase.source: LegacyAnchor → HeaderV2 (serializer emits the
    //     `## Phase X - Title` header form going forward).
    //   - Every task/subtask Node.separator → Hyphen (canonical ` - `).
    //   - Backlog heading h2 → h1 (`# Backlog (not yet phased)`).
    //   - v1 phase state (`[x]`/`[-]`/`[>]` on the anchor) becomes a prose
    //     breadcrumb `*(was marked [x] in v1 — archive to make it official)*`
    //     attached to the phase. The phase.state is then reset to Pending so
    //     v2 round-trip doesn't keep re-emitting the breadcrumb.
    let mut phases_flipped = 0;
    let mut state_breadcrumbs = 0;
    let mut tasks_renormalized = 0;
    for phase in &mut plan.phases {
        if !matches!(phase.source, PhaseSource::HeaderV2) {
            phase.source = PhaseSource::HeaderV2;
            phase.id_style = crate::ast::IdStyle::Plain;
            phase.separator = Separator::Hyphen;
            phases_flipped += 1;
        }
        let mark = match phase.state {
            NodeState::Pending => None,
            NodeState::Done => Some('x'),
            NodeState::WontDo => Some('-'),
            NodeState::Backlog => Some('>'),
        };
        if let Some(m) = mark {
            // Idempotent: only add the breadcrumb when there isn't one
            // already, and clear phase.state to Pending so re-canonicalize
            // doesn't keep re-adding.
            let already_noted = phase.annotations.iter().any(|a| {
                matches!(a, Annotation::Text { text, .. } if text.contains("was marked"))
            });
            if !already_noted {
                phase.annotations.insert(
                    0,
                    Annotation::Text {
                        text: format!(
                            "*(was marked [{m}] in v1 — archive to make it official)*"
                        ),
                        indent: 0,
                    },
                );
                state_breadcrumbs += 1;
            }
            phase.state = NodeState::Pending;
        }
        tasks_renormalized += renormalize_task_separators(&mut phase.children);
    }
    if phases_flipped > 0 {
        notes.push(format!(
            "flipped {phases_flipped} phase(s) from v1 `- [ ] N.0` anchor to v2 `## Phase` header form"
        ));
    }
    if state_breadcrumbs > 0 {
        notes.push(format!(
            "preserved {state_breadcrumbs} v1 phase-state marker(s) as `*(was marked [x] in v1)*` prose notes"
        ));
    }
    if tasks_renormalized > 0 {
        notes.push(format!(
            "normalized {tasks_renormalized} task separator(s) to canonical ` - `"
        ));
    }
    if !plan.backlog.is_empty() && !plan.backlog_h1 {
        plan.backlog_h1 = true;
        notes.push("flipped backlog heading `## Backlog` → `# Backlog` (FORMATv2)".to_string());
    }

    let new_serialized = serialize(&plan);
    let changed = new_serialized != original_serialized;
    if changed && !dry_run {
        std::fs::write(plan_path, &new_serialized)
            .with_context(|| format!("write {}", plan_path.display()))?;
    }
    Ok(CanonicalizeReport {
        notes,
        changed,
        dry_run,
    })
}

/// Force every task/subtask in the slice (and their descendants) to the
/// canonical `Separator::Hyphen`. Bare-id nodes are skipped (no separator to
/// renormalize). Returns the count of nodes touched.
fn renormalize_task_separators(nodes: &mut [crate::ast::Node]) -> usize {
    let mut touched = 0;
    for node in nodes {
        if !node.id.is_empty() && node.separator != Separator::Hyphen {
            node.separator = Separator::Hyphen;
            touched += 1;
        }
        touched += renormalize_task_separators(&mut node.children);
    }
    touched
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::test_utils::write_plan;

    fn scratch_dir() -> PathBuf {
        crate::test_utils::scratch_dir("canonicalize")
    }

    #[test]
    fn canonicalize_promotes_header_phases_to_v2_form() {
        // Phase 37 update: standardize-promoted phases (from legacy
        // `### Phase N — Title` h3 markdown headers in source) now land in
        // FORMATv2 `## Phase N - Title` header form, not the old v1 anchor
        // checkbox. The promotion still happens — just to the new canonical.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 0.1 First\n\n### Phase 1 — Build\n\n- [ ] 1.1 Build it\n",
        );
        let report = canonicalize(&plan, false).unwrap();
        assert!(report.changed);
        assert!(!report.notes.is_empty());
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(
            after.contains("## Phase 1.0 - Build") || after.contains("## Phase 1 - Build"),
            "header phase promoted to v2 header form:\n{after}"
        );
    }

    #[test]
    fn canonicalize_dry_run_leaves_file_unchanged() {
        let dir = scratch_dir();
        let original = "- [ ] 0.1 First\n\n### Phase 1 — Build\n\n- [ ] 1.1 Build it\n";
        let plan = write_plan(&dir, original);
        let report = canonicalize(&plan, true).unwrap();
        assert!(report.changed, "report records what would change");
        assert!(report.dry_run);
        let after = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(after, original, "dry run must not write");
    }

    #[test]
    fn canonicalize_moves_preamble_backlog_to_bottom_and_flips_to_h1() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "# Title\n\n## Backlog (not yet phased)\n\n- **Old** — added 2026-05-01.\n\n- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n",
        );
        let report = canonicalize(&plan, false).unwrap();
        assert!(report.changed);
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("consolidated 1 backlog"))
        );
        let after = std::fs::read_to_string(&plan).unwrap();
        // Phase 37.5: canonicalize flips backlog h2 → h1.
        assert_eq!(after.matches("# Backlog (not yet phased)").count(), 1);
        assert!(
            !after.contains("## Backlog"),
            "h2 backlog should be gone:\n{after}"
        );
        let backlog_pos = after.find("# Backlog").unwrap();
        // v1 phase 1.0 was flipped to `## Phase 1.0 - Phase`; either form
        // means "the phase content sits above the backlog".
        assert!(
            after.contains("## Phase 1.0 - Phase") || after.contains("- [ ] 1.0 Phase"),
            "phase 1.0 emitted in some form:\n{after}"
        );
        let phase_pos = after
            .find("## Phase 1.0")
            .or_else(|| after.find("- [ ] 1.0"))
            .unwrap();
        assert!(
            backlog_pos > phase_pos,
            "backlog should sit below the phases:\n{after}"
        );
        assert!(after.contains("- **Old** — added 2026-05-01."));
    }

    #[test]
    fn canonicalize_noop_on_fully_v2_plan() {
        // Phase 37.5: "already canonical" now means v2 — header phases,
        // hyphen-space separator, h1 backlog. A v2 plan with no v1 vestiges
        // should NOT rewrite the file (changed=false).
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "## Phase AI - Studio\n\n- [ ] AI.0 - first\n\n# Backlog (not yet phased)\n\n- **Note** — added 2026-05-22.\n",
        );
        let report = canonicalize(&plan, false).unwrap();
        assert!(
            !report.changed,
            "fully v2 plan is canonical:\n{:?}",
            report.notes
        );
    }

    #[test]
    fn canonicalize_flips_v1_anchor_to_v2_header() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase title\n  - [ ] 1.1 task\n  - [ ] 1.2 - another task\n",
        );
        let report = canonicalize(&plan, false).unwrap();
        assert!(report.changed);
        assert!(
            report.notes.iter().any(|n| n.contains("flipped 1 phase")),
            "report mentions the phase flip: {:?}",
            report.notes
        );
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(
            after.contains("## Phase 1.0 - Phase title"),
            "v1 anchor flipped to v2 header:\n{after}"
        );
        // Tasks now sit at column 0 under the v2 phase, with hyphen-space
        // separator throughout.
        assert!(
            after.contains("\n- [ ] 1.1 - task\n"),
            "task at column 0 with hyphen-space sep:\n{after}"
        );
        assert!(
            after.contains("\n- [ ] 1.2 - another task\n"),
            "second task too:\n{after}"
        );
    }

    #[test]
    fn canonicalize_preserves_v1_phase_state_as_prose_note() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [x] 1.0 Phase that was ticked\n");
        let report = canonicalize(&plan, false).unwrap();
        assert!(report.changed);
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("preserved 1 v1 phase-state marker")),
            "report mentions state breadcrumb: {:?}",
            report.notes
        );
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(
            after.contains("## Phase 1.0 - Phase that was ticked"),
            "v2 header emitted:\n{after}"
        );
        assert!(
            after.contains("*(was marked [x] in v1 — archive to make it official)*"),
            "v1 state preserved as prose:\n{after}"
        );
    }

    #[test]
    fn canonicalize_is_idempotent_on_second_run() {
        // Running canonicalize twice should be a no-op on the second pass —
        // the flip lands once, then the v2 plan is stable.
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [x] 1.0 Phase title\n  - [ ] 1.1 task\n");
        let r1 = canonicalize(&plan, false).unwrap();
        assert!(r1.changed);
        let after_first = std::fs::read_to_string(&plan).unwrap();
        let r2 = canonicalize(&plan, false).unwrap();
        assert!(
            !r2.changed,
            "second canonicalize must be a no-op:\n{:?}",
            r2.notes
        );
        let after_second = std::fs::read_to_string(&plan).unwrap();
        assert_eq!(after_first, after_second, "file unchanged on second run");
    }
}

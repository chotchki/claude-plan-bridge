use crate::ast::{Node, Plan};
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
    let mut plan = parse(&text).with_context(|| format!("parse {}", plan_path.display()))?;

    // Partition phases into "stay" vs "archive" preserving order.
    let mut keep: Vec<Node> = Vec::new();
    let mut archive: Vec<Node> = Vec::new();
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
        collect_plan_paths(phase, &mut report.archived_plan_paths);
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
    let combined = prepend_archive(&archive_section, &archive_text);

    atomic_write(plan_path, &new_plan_text)
        .with_context(|| format!("write {}", plan_path.display()))?;
    atomic_write(&archive_path, &combined)
        .with_context(|| format!("write {}", archive_path.display()))?;

    // Drop state mappings whose plan_path lives inside any archived subtree.
    let state_path = default_state_path_for(plan_path);
    let mut state = State::load(&state_path)?;
    let archived: std::collections::HashSet<&str> =
        report.archived_plan_paths.iter().map(String::as_str).collect();
    let to_drop: Vec<String> = state
        .mappings
        .iter()
        .filter(|(_, m)| archived.contains(m.plan_path.as_str()))
        .map(|(tid, _)| tid.clone())
        .collect();
    for tid in &to_drop {
        state.remove(tid);
    }
    if !to_drop.is_empty() {
        state.save(&state_path)?;
    }

    Ok(report)
}

fn phase_fully_done(node: &Node) -> bool {
    if node.is_leaf() {
        return node.checked;
    }
    node.children.iter().all(phase_fully_done)
}

fn collect_plan_paths(node: &Node, out: &mut Vec<String>) {
    out.push(node.id.clone());
    for child in &node.children {
        collect_plan_paths(child, out);
    }
}

fn build_archive_section(today: &str, archived: &[Node]) -> String {
    let mut out = format!("## {today}\n\n");
    for phase in archived {
        let temp = Plan {
            preamble: vec![],
            phases: vec![phase.clone()],
        };
        out.push_str(&serialize(&temp));
        out.push('\n');
    }
    out
}

fn prepend_archive(new_section: &str, existing: &str) -> String {
    if existing.is_empty() {
        return new_section.to_string();
    }
    let mut combined = new_section.to_string();
    if !combined.ends_with("\n\n") {
        if combined.ends_with('\n') {
            combined.push('\n');
        } else {
            combined.push_str("\n\n");
        }
    }
    combined.push_str("---\n\n");
    combined.push_str(existing);
    combined
}

fn archive_path_for(plan_path: &Path) -> PathBuf {
    plan_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("PLAN_ARCHIVE.md")
}

fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("no parent for {}", path.display()))?;
    if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents)
        .with_context(|| format!("write tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Mapping, State, default_state_path_for};
    use std::path::PathBuf;

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plan-bridge-archive-{}-{}",
            std::process::id(),
            uniq()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn uniq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    fn write_plan(dir: &Path, contents: &str) -> PathBuf {
        let p = dir.join("PLAN.md");
        std::fs::write(&p, contents).unwrap();
        p
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
    fn parent_unchecked_but_children_all_done_still_archives() {
        // Bridge doesn't auto-tick parents; a phase whose box reads `[ ]` but
        // whose subtree is fully `[x]` should archive based on subtree state.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Parent unchecked\n  - [x] 1.1 Done\n",
        );
        let report = archive(&plan, false, "2026-05-16").unwrap();
        assert_eq!(report.archived_phase_ids, vec!["1.0"]);
    }

    #[test]
    fn prepends_to_existing_archive() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [x] 1.1 Done\n");
        let archive_path = archive_path_for(&plan);
        std::fs::write(
            &archive_path,
            "## 2026-04-01\n\n- [x] 0.0 Earlier work\n",
        )
        .unwrap();
        archive(&plan, false, "2026-05-16").unwrap();
        let archive_text = std::fs::read_to_string(&archive_path).unwrap();
        let pos_new = archive_text.find("## 2026-05-16").expect("new section present");
        let pos_old = archive_text.find("## 2026-04-01").expect("old section preserved");
        assert!(pos_new < pos_old, "newest should be on top");
        assert!(archive_text.contains("0.0 Earlier work"));
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
        state.record("t-archived", Mapping {
            plan_path: "1.1".to_string(),
            last_synced_title: "Done".to_string(),
            last_synced_checked: true,
            last_synced_annotations: vec![],
        });
        state.record("t-elsewhere", Mapping {
            plan_path: "9.9".to_string(),
            last_synced_title: "x".to_string(),
            last_synced_checked: false,
            last_synced_annotations: vec![],
        });
        state.save(&state_path).unwrap();

        archive(&plan, false, "2026-05-16").unwrap();
        let loaded = State::load(&state_path).unwrap();
        assert_eq!(loaded.plan_path("t-archived"), None, "archived mapping should be gone");
        assert_eq!(loaded.plan_path("t-elsewhere"), Some("9.9"), "unrelated mapping should remain");
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
}

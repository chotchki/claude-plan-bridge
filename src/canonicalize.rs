use anyhow::{Context, Result};
use std::path::Path;

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
            "consolidated {swept} backlog item(s) into the bottom `## Backlog (not yet phased)` section"
        ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "plan-bridge-canonicalize-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            N.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_plan(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("PLAN.md");
        std::fs::write(&p, body).unwrap();
        p
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
    fn canonicalize_moves_preamble_backlog_to_bottom() {
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
        assert_eq!(after.matches("## Backlog (not yet phased)").count(), 1);
        assert!(
            after.find("## Backlog").unwrap() > after.find("- [ ] 1.0 Phase").unwrap(),
            "backlog should sit below the phases:\n{after}"
        );
        assert!(after.contains("- **Old** — added 2026-05-01."));
    }

    #[test]
    fn canonicalize_noop_on_already_canonical_plan() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        let report = canonicalize(&plan, false).unwrap();
        assert!(!report.changed);
        assert!(report.notes.is_empty());
    }
}

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
    let (plan, notes) = parsed
        .standardize_to_canonical()
        .map_err(anyhow::Error::msg)?;
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
        let p = std::env::temp_dir().join(format!(
            "plan-bridge-canonicalize-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
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
    fn canonicalize_promotes_header_phases_to_checkboxes() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 0.1 First\n\n### Phase 1 — Build\n\n- [ ] 1.1 Build it\n",
        );
        let report = canonicalize(&plan, false).unwrap();
        assert!(report.changed);
        assert!(!report.notes.is_empty());
        let after = std::fs::read_to_string(&plan).unwrap();
        assert!(after.contains("- [ ] 1.0 Build"));
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
    fn canonicalize_noop_on_already_canonical_plan() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        let report = canonicalize(&plan, false).unwrap();
        assert!(!report.changed);
        assert!(report.notes.is_empty());
    }
}

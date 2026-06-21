//! Phase CE: the phase template — the standard set of beats `phase-new`
//! scaffolds under a fresh phase. A project can override the built-in default
//! by dropping a `PHASE_TEMPLATE.md` at its root; otherwise the default below
//! applies. Templates are a *scaffold*, not a gate: the beats are a starting
//! point to prune/extend per phase, never a required structure.

use std::path::Path;

/// The built-in default phase template — the recurring beats most phases
/// follow. `Plan & breakdown` is where the Implement / Tests beats get
/// decomposed (via `phase-breakdown`); `Review` and `Release` are the human
/// gates that are easiest to forget, so they ride along as explicit reminders.
pub fn default_template() -> Vec<String> {
    [
        "Plan & breakdown",
        "Implement",
        "Tests + docs",
        "Review",
        "Release (bump + tag + push)",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Resolve the template for `project_root`: the bullets of a `PHASE_TEMPLATE.md`
/// at the root if it exists and yields at least one task, otherwise the
/// built-in [`default_template`]. Present-replaces-default (no merge), so a
/// project's file is the whole story.
pub fn load_template(project_root: &Path) -> Vec<String> {
    let path = project_root.join("PHASE_TEMPLATE.md");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let tasks = parse_template(&text);
        if !tasks.is_empty() {
            return tasks;
        }
    }
    default_template()
}

/// Extract task subjects from a `PHASE_TEMPLATE.md`: each `- ` bullet line, in
/// order, is one beat. A leading checkbox marker (`[ ] `, `[x] `, …) is
/// tolerated and stripped, so a literal checklist works as a template too.
/// Non-bullet lines (headings, prose, blanks) are ignored.
fn parse_template(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("- ")?;
            let subject = strip_checkbox(rest).trim();
            (!subject.is_empty()).then(|| subject.to_string())
        })
        .collect()
}

/// Strip a leading `[<one char>] ` checkbox marker if present (`[ ] task` ->
/// `task`); otherwise return the input unchanged.
fn strip_checkbox(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 4 && b[0] == b'[' && b[2] == b']' && b[3] == b' ' {
        &s[4..]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_the_five_beats() {
        let t = default_template();
        assert_eq!(t.len(), 5);
        assert_eq!(t[0], "Plan & breakdown");
        assert_eq!(t[3], "Review");
        assert!(t[4].starts_with("Release"));
    }

    #[test]
    fn parse_plain_and_checkbox_bullets() {
        let text = "# Phase template\n\nIntro prose.\n- Plan\n- [ ] Build\n- [x] Ship it\n";
        assert_eq!(parse_template(text), vec!["Plan", "Build", "Ship it"]);
    }

    #[test]
    fn parse_ignores_non_bullets_and_empties() {
        let text = "## not a bullet\n-no space after dash\n-   \n- \n- Real\n";
        assert_eq!(parse_template(text), vec!["Real"]);
    }

    #[test]
    fn load_falls_back_to_default_when_absent() {
        let dir = crate::test_utils::scratch_dir("phase-template");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load_template(&dir), default_template());
    }

    #[test]
    fn load_uses_project_file_when_present() {
        let dir = crate::test_utils::scratch_dir("phase-template");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("PHASE_TEMPLATE.md"),
            "- Spike\n- Build\n- Verify\n",
        )
        .unwrap();
        assert_eq!(load_template(&dir), vec!["Spike", "Build", "Verify"]);
    }

    #[test]
    fn load_falls_back_when_file_has_no_bullets() {
        let dir = crate::test_utils::scratch_dir("phase-template");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("PHASE_TEMPLATE.md"),
            "# Just a heading\n\nsome prose\n",
        )
        .unwrap();
        assert_eq!(load_template(&dir), default_template());
    }
}

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::Path;

/// What `init` did, for the CLI summary.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct InitReport {
    pub created_plan: bool,
    pub created_settings: bool,
    pub updated_settings: bool,
    pub updated_gitignore: bool,
    pub created_gitignore: bool,
}

const STARTER_PLAN: &str = "\
# PLAN

Describe what you're building.

- [ ] 1.0 Phase one
";

/// Idempotent install of the bridge into the project rooted at `cwd`:
///
/// 1. Scaffold `PLAN.md` with a starter `1.0 Phase one` if missing (or always
///    if `force=true`).
/// 2. Merge plan-bridge hooks into `.claude/settings.json`. Existing
///    `plan-bridge` entries are stripped and replaced — re-running is safe.
/// 3. Append `.claude/plan-bridge-state.json` to `.gitignore` (creating it if
///    necessary).
pub fn init(cwd: &Path, force: bool) -> Result<InitReport> {
    let mut report = InitReport::default();

    let plan_path = cwd.join("PLAN.md");
    if !plan_path.exists() || force {
        std::fs::write(&plan_path, STARTER_PLAN)
            .with_context(|| format!("write {}", plan_path.display()))?;
        report.created_plan = true;
    }

    let claude_dir = cwd.join(".claude");
    std::fs::create_dir_all(&claude_dir)
        .with_context(|| format!("create {}", claude_dir.display()))?;
    let settings_path = claude_dir.join("settings.json");
    let existed = settings_path.exists();
    let existing = if existed {
        let raw = std::fs::read_to_string(&settings_path)
            .with_context(|| format!("read {}", settings_path.display()))?;
        if raw.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str::<Value>(&raw)
                .with_context(|| format!("parse {}", settings_path.display()))?
        }
    } else {
        json!({})
    };
    let merged = merge_hooks(existing);
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&merged)? + "\n",
    )
    .with_context(|| format!("write {}", settings_path.display()))?;
    if existed {
        report.updated_settings = true;
    } else {
        report.created_settings = true;
    }

    let gitignore_path = cwd.join(".gitignore");
    let gi_lines = [
        ".claude/plan-bridge-state.json",
        ".claude/plan-bridge-state.json.lock",
    ];
    let existed_gi = gitignore_path.exists();
    let mut current = if existed_gi {
        std::fs::read_to_string(&gitignore_path)
            .with_context(|| format!("read {}", gitignore_path.display()))?
    } else {
        String::new()
    };
    let mut changed = false;
    for gi_line in &gi_lines {
        let already = current.lines().any(|l| {
            let trimmed = l.trim().trim_start_matches('/');
            trimmed == *gi_line
        });
        if !already {
            if !current.is_empty() && !current.ends_with('\n') {
                current.push('\n');
            }
            current.push_str(gi_line);
            current.push('\n');
            changed = true;
        }
    }
    if changed {
        std::fs::write(&gitignore_path, &current)
            .with_context(|| format!("write {}", gitignore_path.display()))?;
        if existed_gi {
            report.updated_gitignore = true;
        } else {
            report.created_gitignore = true;
        }
    }

    Ok(report)
}

/// Canonical hook entries the bridge installs. Each entry follows Claude
/// Code's hook config shape: `{matcher?, hooks: [{type, command}]}`.
fn plan_bridge_hooks() -> Value {
    json!({
        "UserPromptSubmit": [{
            "hooks": [{
                "type": "command",
                "command": "plan-bridge reconcile",
            }],
        }],
        "PostToolUse": [
            {
                "matcher": "TaskCreate",
                "hooks": [{
                    "type": "command",
                    "command": "plan-bridge writeback --event create",
                }],
            },
            {
                "matcher": "TaskUpdate",
                "hooks": [{
                    "type": "command",
                    "command": "plan-bridge writeback --event update",
                }],
            },
        ],
    })
}

fn merge_hooks(mut existing: Value) -> Value {
    if !existing.is_object() {
        existing = json!({});
    }
    let our = plan_bridge_hooks();

    let target = existing.as_object_mut().expect("settings is object");
    let hooks_entry = target.entry("hooks".to_string()).or_insert(json!({}));
    if !hooks_entry.is_object() {
        *hooks_entry = json!({});
    }
    let hooks_obj = hooks_entry.as_object_mut().expect("hooks is object");

    for (event, our_entries) in our.as_object().expect("our hooks is object") {
        let event_arr = hooks_obj
            .entry(event.clone())
            .or_insert(json!([]));
        if !event_arr.is_array() {
            *event_arr = json!([]);
        }
        let arr = event_arr.as_array_mut().expect("event is array");
        arr.retain(|e| !is_plan_bridge_entry(e));
        for entry in our_entries.as_array().expect("our entries is array") {
            arr.push(entry.clone());
        }
    }

    existing
}

fn is_plan_bridge_entry(entry: &Value) -> bool {
    let Some(hooks) = entry.get("hooks").and_then(Value::as_array) else {
        return false;
    };
    hooks.iter().any(|h| {
        h.get("command")
            .and_then(Value::as_str)
            .is_some_and(|c| c.contains("plan-bridge"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plan-bridge-init-{}-{}",
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

    #[test]
    fn fresh_init_creates_everything() {
        let dir = scratch_dir();
        let report = init(&dir, false).unwrap();
        assert!(report.created_plan);
        assert!(report.created_settings);
        assert!(report.created_gitignore);

        let plan = std::fs::read_to_string(dir.join("PLAN.md")).unwrap();
        assert!(plan.contains("- [ ] 1.0 Phase one"));

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let hooks = settings.get("hooks").unwrap();
        assert!(hooks.get("UserPromptSubmit").is_some());
        assert!(hooks.get("PostToolUse").is_some());

        let gi = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(gi.contains(".claude/plan-bridge-state.json"));
    }

    #[test]
    fn idempotent_re_init_does_not_duplicate_hooks() {
        let dir = scratch_dir();
        init(&dir, false).unwrap();
        init(&dir, false).unwrap();
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let post = settings
            .pointer("/hooks/PostToolUse")
            .and_then(Value::as_array)
            .unwrap();
        let plan_bridge_count = post.iter().filter(|e| is_plan_bridge_entry(e)).count();
        assert_eq!(plan_bridge_count, 2, "expected exactly two plan-bridge PostToolUse entries");
    }

    #[test]
    fn preserves_user_hooks() {
        let dir = scratch_dir();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let existing = json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": "Edit",
                    "hooks": [{
                        "type": "command",
                        "command": "my-user-script /tmp/log"
                    }]
                }]
            }
        });
        std::fs::write(
            dir.join(".claude/settings.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();
        init(&dir, false).unwrap();

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let post = settings
            .pointer("/hooks/PostToolUse")
            .and_then(Value::as_array)
            .unwrap();
        let has_user = post.iter().any(|e| {
            e.get("hooks")
                .and_then(Value::as_array)
                .map(|hs| hs.iter().any(|h| {
                    h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|c| c.contains("my-user-script"))
                }))
                .unwrap_or(false)
        });
        assert!(has_user, "user's hook should survive init");
    }

    #[test]
    fn skips_plan_when_present() {
        let dir = scratch_dir();
        std::fs::write(dir.join("PLAN.md"), "# Custom plan\n").unwrap();
        let report = init(&dir, false).unwrap();
        assert!(!report.created_plan);
        let plan = std::fs::read_to_string(dir.join("PLAN.md")).unwrap();
        assert_eq!(plan, "# Custom plan\n");
    }

    #[test]
    fn force_overwrites_plan() {
        let dir = scratch_dir();
        std::fs::write(dir.join("PLAN.md"), "# Custom\n").unwrap();
        let report = init(&dir, true).unwrap();
        assert!(report.created_plan);
        let plan = std::fs::read_to_string(dir.join("PLAN.md")).unwrap();
        assert!(plan.contains("1.0 Phase one"));
    }

    #[test]
    fn gitignore_idempotent() {
        let dir = scratch_dir();
        std::fs::write(dir.join(".gitignore"), "/target\n").unwrap();
        init(&dir, false).unwrap();
        let report = init(&dir, false).unwrap();
        assert!(!report.created_gitignore);
        assert!(!report.updated_gitignore, "second init should not touch .gitignore");
        let gi = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        let state_lines = gi
            .lines()
            .filter(|l| l.trim().trim_start_matches('/') == ".claude/plan-bridge-state.json")
            .count();
        let lock_lines = gi
            .lines()
            .filter(|l| l.trim().trim_start_matches('/') == ".claude/plan-bridge-state.json.lock")
            .count();
        assert_eq!(state_lines, 1, "state.json line should appear exactly once");
        assert_eq!(lock_lines, 1, "state.json.lock line should appear exactly once");
    }
}

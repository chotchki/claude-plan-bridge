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

/// What `upgrade_hooks` did, for the CLI summary.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct UpgradeHooksReport {
    pub created_settings: bool,
    pub updated_settings: bool,
    /// True when the merged settings equal the existing ones (no real change).
    pub no_change: bool,
}

const STARTER_PLAN: &str = "\
# PLAN

Describe what you're building.

<!--
This PLAN.md is driven by `claude-plan-bridge`:
- TaskCreate adds a `- [ ] N.M task` line (auto-managed `Inbox.0` if no
  `metadata.plan_path`).
- TaskUpdate(status='completed') ticks the box; (status='deleted') removes
  the line; (subject='...') rewrites the title.
- Hand-edits between turns surface as `additionalContext` on the next
  prompt — the bridge reconciles on every UserPromptSubmit.
- `claude-plan-bridge archive` sweeps fully-`[x]` top-level phases into
  PLAN_ARCHIVE.md.
- `claude-plan-bridge status` reports state-file health if something
  looks wrong.
-->

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
        "SessionStart": [{
            "hooks": [{
                "type": "command",
                "command": "claude-plan-bridge resume",
            }],
        }],
        "UserPromptSubmit": [{
            "hooks": [{
                "type": "command",
                "command": "claude-plan-bridge reconcile",
            }],
        }],
        "PostToolUse": [
            {
                "matcher": "TaskCreate",
                "hooks": [{
                    "type": "command",
                    "command": "claude-plan-bridge writeback --event create",
                }],
            },
            {
                "matcher": "TaskUpdate",
                "hooks": [{
                    "type": "command",
                    "command": "claude-plan-bridge writeback --event update",
                }],
            },
        ],
    })
}

/// Detect whether the project at `cwd` has the SessionStart → resume hook
/// installed. Returns a warning string when the hook is missing (or the
/// settings.json is unreadable / lacks the entry); returns `None` when the
/// hook is present.
///
/// Used by writeback and reconcile to yell loudly on every hook fire so a
/// user on a pre-25.2 install discovers the missing hook the next time
/// they create or update a task — without us needing to remember to run
/// anything.
pub fn missing_session_start_warning(cwd: &Path) -> Option<String> {
    let settings_path = cwd.join(".claude/settings.json");
    let Ok(text) = std::fs::read_to_string(&settings_path) else {
        // Settings file unreadable or missing — most likely running outside
        // a configured project (e.g., CLI smoke test). Stay quiet.
        return None;
    };
    if text.contains("claude-plan-bridge resume") {
        return None;
    }
    Some(
        "claude-plan-bridge: ⚠ SessionStart hook missing from .claude/settings.json — \
         task list won't rehydrate automatically when you restart Claude Code. Run \
         `claude-plan-bridge upgrade-hooks` to add it (idempotent)."
            .to_string(),
    )
}

/// Idempotently merge the latest plan-bridge hooks into an existing
/// `.claude/settings.json` without touching PLAN.md or `.gitignore`. Use
/// this on projects that installed with an older bridge version that
/// didn't ship every hook entry — e.g., projects predating the
/// SessionStart hook introduced in 25.2.
pub fn upgrade_hooks(cwd: &Path) -> Result<UpgradeHooksReport> {
    let mut report = UpgradeHooksReport::default();
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
    let merged = merge_hooks(existing.clone());
    if existed && merged == existing {
        report.no_change = true;
        return Ok(report);
    }
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
    Ok(report)
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
        let event_arr = hooks_obj.entry(event.clone()).or_insert(json!([]));
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
            .is_some_and(|c| c.contains("claude-plan-bridge"))
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
        assert!(
            hooks.get("SessionStart").is_some(),
            "SessionStart hook missing"
        );
        assert!(hooks.get("UserPromptSubmit").is_some());
        assert!(hooks.get("PostToolUse").is_some());

        let gi = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(gi.contains(".claude/plan-bridge-state.json"));
    }

    #[test]
    fn fresh_init_includes_session_start_resume_hook() {
        // Phase 25.2: SessionStart hook must run `claude-plan-bridge resume`
        // so a fresh Claude Code session rehydrates the task list from the
        // state file.
        let dir = scratch_dir();
        init(&dir, false).unwrap();
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let ss = settings
            .pointer("/hooks/SessionStart")
            .and_then(Value::as_array)
            .expect("SessionStart array");
        let has_resume = ss.iter().any(|e| {
            e.get("hooks")
                .and_then(Value::as_array)
                .map(|hs| {
                    hs.iter().any(|h| {
                        h.get("command").and_then(Value::as_str)
                            == Some("claude-plan-bridge resume")
                    })
                })
                .unwrap_or(false)
        });
        assert!(has_resume, "SessionStart hook missing resume command");
    }

    #[test]
    fn upgrade_hooks_patches_pre_25_2_install() {
        // Simulate an existing install from before 25.2: settings.json has
        // the old three hooks but no SessionStart entry. upgrade_hooks
        // should add it without disturbing the others.
        let dir = scratch_dir();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let pre_25_2 = json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": "claude-plan-bridge reconcile"
                    }]
                }],
                "PostToolUse": [
                    {
                        "matcher": "TaskCreate",
                        "hooks": [{
                            "type": "command",
                            "command": "claude-plan-bridge writeback --event create"
                        }]
                    },
                    {
                        "matcher": "TaskUpdate",
                        "hooks": [{
                            "type": "command",
                            "command": "claude-plan-bridge writeback --event update"
                        }]
                    }
                ]
            }
        });
        std::fs::write(
            dir.join(".claude/settings.json"),
            serde_json::to_string_pretty(&pre_25_2).unwrap(),
        )
        .unwrap();

        let report = upgrade_hooks(&dir).unwrap();
        assert!(report.updated_settings, "expected updated_settings");
        assert!(!report.no_change);

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert!(
            settings.pointer("/hooks/SessionStart").is_some(),
            "SessionStart missing after upgrade"
        );
        // PostToolUse entries should still be there, with no duplicates.
        let post = settings
            .pointer("/hooks/PostToolUse")
            .and_then(Value::as_array)
            .unwrap();
        let plan_bridge_count = post.iter().filter(|e| is_plan_bridge_entry(e)).count();
        assert_eq!(plan_bridge_count, 2);
    }

    #[test]
    fn upgrade_hooks_is_idempotent_no_change_on_second_run() {
        let dir = scratch_dir();
        init(&dir, false).unwrap();
        // PLAN.md and gitignore got created by init; upgrade should be a no-op.
        let report = upgrade_hooks(&dir).unwrap();
        assert!(
            report.no_change,
            "expected no_change report on already-current settings"
        );
        assert!(!report.created_settings);
        assert!(!report.updated_settings);
    }

    #[test]
    fn missing_session_start_warning_returns_none_when_hook_present() {
        let dir = scratch_dir();
        init(&dir, false).unwrap();
        assert!(missing_session_start_warning(&dir).is_none());
    }

    #[test]
    fn missing_session_start_warning_fires_for_pre_25_2_install() {
        let dir = scratch_dir();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let pre_25_2 = json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": "claude-plan-bridge reconcile"
                    }]
                }]
            }
        });
        std::fs::write(
            dir.join(".claude/settings.json"),
            serde_json::to_string_pretty(&pre_25_2).unwrap(),
        )
        .unwrap();
        let warn = missing_session_start_warning(&dir).expect("expected warning");
        assert!(warn.contains("SessionStart"), "warn: {warn}");
        assert!(warn.contains("upgrade-hooks"), "warn: {warn}");
    }

    #[test]
    fn missing_session_start_warning_silent_when_settings_absent() {
        // Running outside a configured Claude Code project (e.g. CLI smoke
        // test) shouldn't emit warnings.
        let dir = scratch_dir();
        assert!(missing_session_start_warning(&dir).is_none());
    }

    #[test]
    fn upgrade_hooks_creates_settings_when_missing() {
        let dir = scratch_dir();
        let report = upgrade_hooks(&dir).unwrap();
        assert!(report.created_settings);
        assert!(!report.no_change);
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert!(settings.pointer("/hooks/SessionStart").is_some());
        // upgrade_hooks must NOT scaffold PLAN.md or .gitignore.
        assert!(
            !dir.join("PLAN.md").exists(),
            "upgrade_hooks scaffolded PLAN.md"
        );
        assert!(
            !dir.join(".gitignore").exists(),
            "upgrade_hooks scaffolded .gitignore"
        );
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
        assert_eq!(
            plan_bridge_count, 2,
            "expected exactly two plan-bridge PostToolUse entries"
        );
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
                .map(|hs| {
                    hs.iter().any(|h| {
                        h.get("command")
                            .and_then(Value::as_str)
                            .is_some_and(|c| c.contains("my-user-script"))
                    })
                })
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
        assert!(
            !report.updated_gitignore,
            "second init should not touch .gitignore"
        );
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
        assert_eq!(
            lock_lines, 1,
            "state.json.lock line should appear exactly once"
        );
    }
}

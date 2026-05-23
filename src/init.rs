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
- TaskCreate adds a `- [ ] N.M task` line at `metadata.plan_path`; with no
  `plan_path` it lands as a tracked note in the bottom `## Backlog (not yet
  phased)` section instead.
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
    let abs_cwd = absolute_project_root(cwd)?;
    let merged = merge_hooks(existing, &abs_cwd);
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

/// Resolve the project root to a canonical absolute path. Hook commands bake
/// this into `--cwd` so the subprocess's working directory (which can drift
/// if Claude `cd`s mid-session) doesn't determine where the bridge looks for
/// PLAN.md. Falls back to joining `cwd` onto `current_dir()` if
/// `canonicalize` fails (rare — directory must exist for init/upgrade).
pub fn absolute_project_root(cwd: &Path) -> Result<std::path::PathBuf> {
    if let Ok(canon) = std::fs::canonicalize(cwd) {
        return Ok(canon);
    }
    let here = std::env::current_dir().context("current_dir")?;
    Ok(here.join(cwd))
}

/// Quote a path for embedding into a shell command string. Hook commands are
/// run through `sh -c`, so a project root containing spaces, `$`, or other
/// shell metacharacters would otherwise break tokenization. Single-quote +
/// escape internal single quotes (`'` → `'\''`).
pub fn shell_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Canonical hook entries the bridge installs. Each command bakes an absolute
/// `--cwd <project root>` so the subprocess CWD is irrelevant — see Phase 32
/// (cwd-drift session implode).
fn plan_bridge_hooks(project_root: &Path) -> Value {
    let q = shell_quote(project_root);
    // NOTE: `--cwd` is a subcommand-level flag (it lives on `ProjectArgs`,
    // which clap flattens into each subcommand), so it MUST appear AFTER
    // the subcommand token. Putting it before makes clap reject with
    // "unexpected argument '--cwd' found". Phase 32 v0.1.20 shipped with
    // the wrong order; the bridge auto-detects and migrates.
    json!({
        "SessionStart": [{
            "hooks": [{
                "type": "command",
                "command": format!("claude-plan-bridge resume --cwd {q}"),
            }],
        }],
        "UserPromptSubmit": [{
            "hooks": [{
                "type": "command",
                "command": format!("claude-plan-bridge reconcile --cwd {q}"),
            }],
        }],
        "PostToolUse": [
            {
                "matcher": "TaskCreate",
                "hooks": [{
                    "type": "command",
                    "command": format!("claude-plan-bridge writeback --event create --cwd {q}"),
                }],
            },
            {
                "matcher": "TaskUpdate",
                "hooks": [{
                    "type": "command",
                    "command": format!("claude-plan-bridge writeback --event update --cwd {q}"),
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
/// When the installed plan-bridge hook entries don't all carry an absolute
/// `--cwd <abs>`, return a non-blocking warning telling the user to run
/// `upgrade-hooks`. Phase 32: legacy installs are vulnerable to subprocess
/// cwd drift (Claude `cd`'ing mid-session) which prior to 32.1 also
/// hard-blocked every prompt. Even with the defensive guard, a bridge that
/// silently no-ops on the wrong cwd isn't fixing the user's plan — they need
/// to migrate.
pub fn outdated_hook_cwd_warning(cwd: &Path) -> Option<String> {
    let settings_path = cwd.join(".claude/settings.json");
    let text = std::fs::read_to_string(&settings_path).ok()?;
    let settings: Value = serde_json::from_str(&text).ok()?;
    if settings
        .pointer("/hooks")
        .and_then(Value::as_object)
        .is_none_or(|h| h.is_empty())
    {
        return None;
    }
    // If there are no plan-bridge entries at all, stay quiet — most likely
    // running outside a configured project.
    let any_plan_bridge = settings
        .pointer("/hooks")
        .and_then(Value::as_object)
        .map(|hooks| {
            hooks.values().any(|entries| {
                entries
                    .as_array()
                    .map(|arr| arr.iter().any(is_plan_bridge_entry))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if !any_plan_bridge {
        return None;
    }
    if hooks_have_absolute_cwd(&settings) {
        return None;
    }
    Some(
        "claude-plan-bridge: ⚠ installed hook entries are using a relative \
         (or absent) `--cwd` flag. If Claude `cd`s into a subdirectory \
         mid-session, the hook subprocess inherits that cwd and the bridge \
         silently no-ops against PLAN.md until the session restarts. Run \
         `claude-plan-bridge upgrade-hooks` to bake the absolute project root \
         into every hook command (idempotent)."
            .to_string(),
    )
}

pub fn missing_session_start_warning(cwd: &Path) -> Option<String> {
    let settings_path = cwd.join(".claude/settings.json");
    let Ok(text) = std::fs::read_to_string(&settings_path) else {
        // Settings file unreadable or missing — most likely running outside
        // a configured project (e.g., CLI smoke test). Stay quiet.
        return None;
    };
    let Ok(settings) = serde_json::from_str::<Value>(&text) else {
        return None;
    };
    if hook_command_present(&settings, "SessionStart", "resume") {
        return None;
    }
    Some(
        "claude-plan-bridge: ⚠ SessionStart hook missing from .claude/settings.json — \
         task list won't rehydrate automatically when you restart Claude Code. Run \
         `claude-plan-bridge upgrade-hooks` to add it (idempotent)."
            .to_string(),
    )
}

/// Walk `settings.hooks.<event>` arrays looking for a plan-bridge command that
/// includes `subcommand` as a whitespace-separated token. Handles both legacy
/// commands (`claude-plan-bridge resume`) and Phase 32 commands with absolute
/// `--cwd <path>` baked in (`claude-plan-bridge --cwd '/abs' resume`).
pub fn hook_command_present(settings: &Value, event: &str, subcommand: &str) -> bool {
    let Some(arr) = settings
        .pointer(&format!("/hooks/{event}"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    arr.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(Value::as_array)
            .map(|hs| {
                hs.iter().any(|h| {
                    let Some(cmd) = h.get("command").and_then(Value::as_str) else {
                        return false;
                    };
                    cmd.contains("claude-plan-bridge")
                        && cmd.split_whitespace().any(|t| t == subcommand)
                })
            })
            .unwrap_or(false)
    })
}

/// True when *every* installed plan-bridge hook command bakes an absolute
/// `--cwd` flag. Used by reconcile/writeback to detect legacy installs that
/// still rely on subprocess cwd to resolve PLAN.md — Phase 32.
pub fn hooks_have_absolute_cwd(settings: &Value) -> bool {
    let Some(hooks) = settings.pointer("/hooks").and_then(Value::as_object) else {
        return false;
    };
    let mut saw_any = false;
    for entries in hooks.values() {
        let Some(arr) = entries.as_array() else {
            continue;
        };
        for entry in arr {
            let Some(hs) = entry.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for h in hs {
                let Some(cmd) = h.get("command").and_then(Value::as_str) else {
                    continue;
                };
                if !cmd.contains("claude-plan-bridge") {
                    continue;
                }
                saw_any = true;
                if !command_has_absolute_cwd(cmd) {
                    return false;
                }
            }
        }
    }
    saw_any
}

/// True when `cmd` carries `--cwd <abs path>` where the path is absolute.
/// Tokenizes on whitespace; tolerates single-quoted paths from `shell_quote`.
fn command_has_absolute_cwd(cmd: &str) -> bool {
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    for (i, t) in toks.iter().enumerate() {
        if *t != "--cwd" {
            continue;
        }
        let Some(arg) = toks.get(i + 1) else {
            return false;
        };
        // `shell_quote` wraps in single quotes; strip them for the check.
        let unquoted = arg.trim_start_matches('\'').trim_end_matches('\'');
        return std::path::Path::new(unquoted).is_absolute();
    }
    false
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
    let abs_cwd = absolute_project_root(cwd)?;
    let merged = merge_hooks(existing.clone(), &abs_cwd);
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

fn merge_hooks(mut existing: Value, project_root: &Path) -> Value {
    if !existing.is_object() {
        existing = json!({});
    }
    let our = plan_bridge_hooks(project_root);

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
        crate::test_utils::scratch_dir("init")
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
        assert!(
            hook_command_present(&settings, "SessionStart", "resume"),
            "SessionStart hook missing resume command"
        );
    }

    #[test]
    fn fresh_init_bakes_absolute_cwd_into_every_hook_command() {
        // Phase 32.2: every installed hook command must carry `--cwd <abs>`
        // so subprocess cwd drift (Claude `cd`s mid-session) doesn't break
        // PLAN.md resolution.
        //
        // Phase 34.1: walk the parsed settings instead of doing a raw-byte
        // substring against the JSON file. On Windows the canonical path
        // is `\\?\C:\...` and JSON serialization escapes every backslash,
        // so the on-disk bytes (`\\\\?\\C:\\...`) never match the
        // unescaped `abs_str`. The parsed `command` field is already
        // unescaped, so comparing against that works on both platforms.
        let dir = scratch_dir();
        init(&dir, false).unwrap();
        let abs = std::fs::canonicalize(&dir).unwrap();
        let abs_str = abs.to_string_lossy().to_string();
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert!(
            hooks_have_absolute_cwd(&settings),
            "expected every plan-bridge hook command to carry --cwd <abs>: {settings:#?}"
        );
        let expected = format!("--cwd '{abs_str}'");
        let hooks = settings
            .pointer("/hooks")
            .and_then(Value::as_object)
            .expect("settings has /hooks");
        let mut checked = 0;
        for entries in hooks.values() {
            for entry in entries.as_array().expect("hook event value is an array") {
                for h in entry
                    .get("hooks")
                    .and_then(Value::as_array)
                    .expect("entry has a hooks array")
                {
                    let cmd = h
                        .get("command")
                        .and_then(Value::as_str)
                        .expect("hook entry has a command string");
                    assert!(
                        cmd.contains(&expected),
                        "command missing `{expected}`: {cmd}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(
            checked >= 4,
            "expected at least 4 plan-bridge hook commands, got {checked}"
        );
    }

    #[test]
    fn fresh_init_puts_subcommand_before_cwd_flag() {
        // `--cwd` is a subcommand-level flag (lives on ProjectArgs which clap
        // flattens into each subcommand). It MUST appear AFTER the subcommand
        // token, otherwise clap rejects with "unexpected argument '--cwd'
        // found". Regression guard for the v0.1.20 ordering bug.
        let dir = scratch_dir();
        init(&dir, false).unwrap();
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let mut checked = 0;
        if let Some(hooks) = settings.pointer("/hooks").and_then(Value::as_object) {
            for entries in hooks.values() {
                let Some(arr) = entries.as_array() else {
                    continue;
                };
                for entry in arr {
                    let Some(hs) = entry.get("hooks").and_then(Value::as_array) else {
                        continue;
                    };
                    for h in hs {
                        let Some(cmd) = h.get("command").and_then(Value::as_str) else {
                            continue;
                        };
                        if !cmd.contains("claude-plan-bridge") {
                            continue;
                        }
                        let toks: Vec<&str> = cmd.split_whitespace().collect();
                        // toks[0] is the binary, toks[1] must be a subcommand
                        // (resume/reconcile/writeback), NOT `--cwd`.
                        assert_ne!(
                            toks.get(1).copied(),
                            Some("--cwd"),
                            "hook command puts --cwd before the subcommand, will be rejected by clap: {cmd}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(
            checked >= 4,
            "expected at least 4 plan-bridge hook entries checked, got {checked}"
        );
    }

    #[test]
    fn upgrade_hooks_migrates_relative_cwd_to_absolute() {
        // Phase 32.3: pre-32 installs ship `claude-plan-bridge reconcile`
        // (no --cwd). After upgrade_hooks, every command bakes the absolute
        // project root, and the install self-heals against cwd drift.
        let dir = scratch_dir();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let legacy = json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{
                        "type": "command",
                        "command": "claude-plan-bridge resume"
                    }]
                }],
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
            serde_json::to_string_pretty(&legacy).unwrap(),
        )
        .unwrap();

        let report = upgrade_hooks(&dir).unwrap();
        assert!(report.updated_settings, "expected updated_settings");
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert!(
            hooks_have_absolute_cwd(&settings),
            "post-upgrade settings still lack absolute --cwd: {settings:#?}"
        );

        // Second upgrade is a no-op.
        let report2 = upgrade_hooks(&dir).unwrap();
        assert!(
            report2.no_change,
            "expected idempotent no-op on second upgrade, got: {report2:?}"
        );
    }

    #[test]
    fn shell_quote_handles_paths_with_spaces_and_quotes() {
        use std::path::PathBuf;
        assert_eq!(shell_quote(&PathBuf::from("/tmp/abc")), "'/tmp/abc'");
        assert_eq!(
            shell_quote(&PathBuf::from("/tmp/with space")),
            "'/tmp/with space'"
        );
        assert_eq!(
            shell_quote(&PathBuf::from("/tmp/o'reilly")),
            "'/tmp/o'\\''reilly'"
        );
    }

    #[test]
    fn hooks_have_absolute_cwd_rejects_relative() {
        let settings = json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": "claude-plan-bridge reconcile"
                    }]
                }]
            }
        });
        assert!(!hooks_have_absolute_cwd(&settings));
    }

    #[test]
    fn hooks_have_absolute_cwd_accepts_absolute() {
        // Phase 34.2: pick a platform-appropriate absolute path. Windows
        // requires a drive letter — `Path::new("/abs/path").is_absolute()`
        // returns false there — so the synthetic command must use the
        // local platform's absolute-path syntax. The function under test
        // delegates to `Path::is_absolute`, which is the correct behavior;
        // the test just needs an honestly-absolute example per platform.
        let abs = if cfg!(windows) {
            r"C:\abs\path"
        } else {
            "/abs/path"
        };
        let cmd = format!("claude-plan-bridge --cwd '{abs}' reconcile");
        let settings = json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": cmd
                    }]
                }]
            }
        });
        assert!(hooks_have_absolute_cwd(&settings));
    }

    #[test]
    fn hooks_have_absolute_cwd_rejects_when_cwd_arg_relative() {
        // A bogus `--cwd .` shouldn't pass — relative paths are exactly what
        // Phase 32 is trying to eliminate.
        let settings = json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": "claude-plan-bridge --cwd . reconcile"
                    }]
                }]
            }
        });
        assert!(!hooks_have_absolute_cwd(&settings));
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
    fn outdated_hook_cwd_warning_fires_for_legacy_install() {
        let dir = scratch_dir();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let legacy = json!({
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
            serde_json::to_string_pretty(&legacy).unwrap(),
        )
        .unwrap();
        let warn = outdated_hook_cwd_warning(&dir).expect("expected warning");
        assert!(warn.contains("upgrade-hooks"), "warn: {warn}");
        assert!(warn.contains("--cwd"), "warn: {warn}");
    }

    #[test]
    fn outdated_hook_cwd_warning_silent_after_init() {
        let dir = scratch_dir();
        init(&dir, false).unwrap();
        assert!(outdated_hook_cwd_warning(&dir).is_none());
    }

    #[test]
    fn outdated_hook_cwd_warning_silent_when_no_plan_bridge_hooks() {
        // User has unrelated hooks installed but no plan-bridge ones.
        // Shouldn't nag them.
        let dir = scratch_dir();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let user_only = json!({
            "hooks": {
                "Notification": [{
                    "hooks": [{
                        "type": "command",
                        "command": "afplay /System/Library/Sounds/Ping.aiff"
                    }]
                }]
            }
        });
        std::fs::write(
            dir.join(".claude/settings.json"),
            serde_json::to_string_pretty(&user_only).unwrap(),
        )
        .unwrap();
        assert!(outdated_hook_cwd_warning(&dir).is_none());
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

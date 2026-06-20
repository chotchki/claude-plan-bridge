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

/// The drift-proof, checkout-portable project-root reference baked into every
/// hook command. Claude Code sets `$CLAUDE_PROJECT_DIR` to the absolute project
/// root for every hook event (SessionStart / UserPromptSubmit / PostToolUse),
/// independent of the subprocess cwd. Using it instead of a hard-coded absolute
/// path fixes BOTH historical failure modes:
///   - Phase 32 (cwd drift): a relative/absent `--cwd` broke when Claude `cd`d
///     into a subdirectory mid-session. `$CLAUDE_PROJECT_DIR` always resolves
///     to the project root regardless of subprocess cwd.
///   - Phase CA (checkout portability): a machine-specific absolute path baked
///     into a committed `.claude/settings.json` broke on every rename, fresh
///     clone, other machine, and git worktree. The variable reference is
///     byte-identical across all checkouts, so the file is safe to commit.
///
/// Double-quoted so the shell expands the variable (single quotes would not).
/// `claude-plan-bridge` additionally falls back to the env var / current dir
/// when `--cwd` arrives empty (the rare headless context where the var is
/// unset), so a misconfigured hook fails loudly rather than silently
/// mis-resolving PLAN.md.
const HOOK_CWD: &str = "\"$CLAUDE_PROJECT_DIR\"";

/// Canonical hook entries the bridge installs. Each command passes
/// `--cwd "$CLAUDE_PROJECT_DIR"` so the subprocess CWD is irrelevant and the
/// committed settings.json is portable across checkouts — see [`HOOK_CWD`].
fn plan_bridge_hooks() -> Value {
    // NOTE: `--cwd` is a subcommand-level flag (it lives on `ProjectArgs`,
    // which clap flattens into each subcommand), so it MUST appear AFTER
    // the subcommand token. Putting it before makes clap reject with
    // "unexpected argument '--cwd' found". Phase 32 v0.1.20 shipped with
    // the wrong order; the bridge auto-detects and migrates.
    json!({
        "SessionStart": [{
            "hooks": [{
                "type": "command",
                "command": format!("claude-plan-bridge resume --cwd {HOOK_CWD}"),
            }],
        }],
        "UserPromptSubmit": [{
            "hooks": [{
                "type": "command",
                "command": format!("claude-plan-bridge reconcile --cwd {HOOK_CWD}"),
            }],
        }],
        "PostToolUse": [
            {
                "matcher": "TaskCreate",
                "hooks": [{
                    "type": "command",
                    "command": format!("claude-plan-bridge writeback --event create --cwd {HOOK_CWD}"),
                }],
            },
            {
                "matcher": "TaskUpdate",
                "hooks": [{
                    "type": "command",
                    "command": format!("claude-plan-bridge writeback --event update --cwd {HOOK_CWD}"),
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
    if hooks_have_drift_proof_cwd(&settings) {
        return None;
    }
    Some(
        "claude-plan-bridge: ⚠ installed hook entries are using a relative \
         (or absent) `--cwd` flag. If Claude `cd`s into a subdirectory \
         mid-session, the hook subprocess inherits that cwd and the bridge \
         silently no-ops against PLAN.md until the session restarts. Run \
         `claude-plan-bridge upgrade-hooks` to rewrite every hook command to \
         `--cwd \"$CLAUDE_PROJECT_DIR\"` — drift-proof and portable across \
         checkouts (idempotent)."
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

/// True when *every* installed plan-bridge hook command resolves the project
/// root in a drift-proof way: each `--cwd` is either a `$CLAUDE_PROJECT_DIR`
/// reference (Phase CA portable form) or an absolute path (legacy Phase 32
/// baked form). Used by reconcile / writeback / status to detect legacy
/// installs that still rely on subprocess cwd to resolve PLAN.md.
pub fn hooks_have_drift_proof_cwd(settings: &Value) -> bool {
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
                if !command_has_drift_proof_cwd(cmd) {
                    return false;
                }
            }
        }
    }
    saw_any
}

/// True when `cmd` carries a `--cwd` whose argument resolves to the project
/// root regardless of subprocess cwd — either a `$CLAUDE_PROJECT_DIR`
/// reference (the portable form `init` / `upgrade-hooks` now emit) or an
/// absolute filesystem path (legacy baked form). Tokenizes on whitespace;
/// tolerates single- or double-quoted arguments.
fn command_has_drift_proof_cwd(cmd: &str) -> bool {
    match cwd_arg_of(cmd) {
        Some(arg) => cwd_arg_is_drift_proof(arg),
        None => false,
    }
}

/// Extract the raw token following `--cwd` in a hook command string (still
/// quoted as it appears on disk). Returns `None` when there is no `--cwd` or
/// nothing follows it.
fn cwd_arg_of(cmd: &str) -> Option<&str> {
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    let pos = toks.iter().position(|t| *t == "--cwd")?;
    toks.get(pos + 1).copied()
}

/// Diagnose installed plan-bridge hooks whose baked absolute `--cwd` points
/// somewhere broken — a directory that no longer exists, or one with no
/// PLAN.md. This is the exact failure that silently no-ops the bridge when a
/// committed settings.json carries another machine's (or a renamed repo's)
/// path. Returns one message per distinct broken path. The portable
/// `$CLAUDE_PROJECT_DIR` form has no fixed path to check and is skipped.
pub fn stale_baked_cwd_warnings(settings: &Value) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    let Some(hooks) = settings.pointer("/hooks").and_then(Value::as_object) else {
        return out;
    };
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
                let Some(arg) = cwd_arg_of(cmd) else {
                    continue;
                };
                let unquoted = arg.trim_matches(|c| c == '\'' || c == '"');
                if unquoted.contains("CLAUDE_PROJECT_DIR") {
                    continue;
                }
                let p = std::path::Path::new(unquoted);
                if !p.is_absolute() || !seen.insert(unquoted.to_string()) {
                    continue;
                }
                if !p.exists() {
                    out.push(format!(
                        "hook `--cwd` points at a directory that no longer exists: {unquoted}"
                    ));
                } else if !p.join("PLAN.md").exists() {
                    out.push(format!("hook `--cwd` directory has no PLAN.md: {unquoted}"));
                }
            }
        }
    }
    out
}

/// Classify a raw `--cwd` argument token (as it appears in the hook command
/// string, possibly still quoted). A `$CLAUDE_PROJECT_DIR` reference resolves
/// to the project root in any subprocess cwd; so does an absolute path. A
/// relative token (`.`, `sub/dir`) does not.
fn cwd_arg_is_drift_proof(arg: &str) -> bool {
    let unquoted = arg.trim_matches(|c| c == '\'' || c == '"');
    if unquoted.contains("CLAUDE_PROJECT_DIR") {
        return true;
    }
    std::path::Path::new(unquoted).is_absolute()
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
    fn fresh_init_uses_claude_project_dir_in_every_hook_command() {
        // Phase CA: every installed hook command must carry
        // `--cwd "$CLAUDE_PROJECT_DIR"`. That one form is BOTH drift-proof
        // (Claude `cd`ing mid-session can't change where PLAN.md resolves —
        // the Phase 32 concern) AND portable across checkouts (no
        // machine-specific absolute path baked into a committed
        // settings.json — the Phase CA bug). The generated string is
        // byte-identical no matter where the project lives on disk.
        let dir = scratch_dir();
        init(&dir, false).unwrap();
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert!(
            hooks_have_drift_proof_cwd(&settings),
            "expected every plan-bridge hook command to resolve a drift-proof cwd: {settings:#?}"
        );
        let expected = "--cwd \"$CLAUDE_PROJECT_DIR\"";
        let machine_path = dir.to_string_lossy().to_string();
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
                        cmd.contains(expected),
                        "command missing `{expected}`: {cmd}"
                    );
                    assert!(
                        !cmd.contains(&machine_path),
                        "command must NOT bake the machine-specific project path \
                         `{machine_path}`: {cmd}"
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
    fn upgrade_hooks_migrates_baked_absolute_to_project_dir() {
        // Phase CA: a Phase-32-era install baked a machine-specific absolute
        // `--cwd '/some/abs/path'` into every command. That works locally but
        // breaks the moment the repo is renamed or cloned elsewhere — the bug
        // that started this phase (this very repo carried a stale
        // `/Users/.../plan_to_task_bridge` after a rename). upgrade_hooks must
        // rewrite every command to the portable `--cwd "$CLAUDE_PROJECT_DIR"`
        // form and drop the stale absolute path entirely.
        let dir = scratch_dir();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let stale_abs = if cfg!(windows) {
            r"C:\old\checkout\path"
        } else {
            "/old/checkout/path"
        };
        let legacy = json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{
                        "type": "command",
                        "command": format!("claude-plan-bridge resume --cwd '{stale_abs}'")
                    }]
                }],
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": format!("claude-plan-bridge reconcile --cwd '{stale_abs}'")
                    }]
                }],
                "PostToolUse": [
                    {
                        "matcher": "TaskCreate",
                        "hooks": [{
                            "type": "command",
                            "command": format!("claude-plan-bridge writeback --event create --cwd '{stale_abs}'")
                        }]
                    },
                    {
                        "matcher": "TaskUpdate",
                        "hooks": [{
                            "type": "command",
                            "command": format!("claude-plan-bridge writeback --event update --cwd '{stale_abs}'")
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
        let raw = std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap();
        assert!(
            !raw.contains(stale_abs),
            "stale absolute path survived the upgrade: {raw}"
        );
        let settings: Value = serde_json::from_str(&raw).unwrap();
        assert!(
            hooks_have_drift_proof_cwd(&settings),
            "post-upgrade settings still lack a drift-proof cwd: {settings:#?}"
        );

        // Second upgrade is a no-op.
        let report2 = upgrade_hooks(&dir).unwrap();
        assert!(
            report2.no_change,
            "expected idempotent no-op on second upgrade, got: {report2:?}"
        );
    }

    #[test]
    fn upgrade_hooks_collapses_duplicate_plan_bridge_entries() {
        // A settings.json that somehow accumulated DUPLICATE plan-bridge
        // entries for an event (a past merge that appended instead of
        // replacing, or two installs that wired different absolute paths)
        // must collapse to exactly one entry per event after upgrade.
        // merge_hooks matches plan-bridge entries by the `claude-plan-bridge`
        // command prefix, retains everything else, and re-adds one canonical
        // set.
        let dir = scratch_dir();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let dupes = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "hooks": [{
                            "type": "command",
                            "command": "claude-plan-bridge reconcile --cwd '/old/path/a'"
                        }]
                    },
                    {
                        "hooks": [{
                            "type": "command",
                            "command": "claude-plan-bridge reconcile --cwd '/old/path/b'"
                        }]
                    }
                ],
                "PostToolUse": [
                    {
                        "matcher": "TaskCreate",
                        "hooks": [{
                            "type": "command",
                            "command": "claude-plan-bridge writeback --event create --cwd '/old/path/a'"
                        }]
                    },
                    {
                        "matcher": "TaskCreate",
                        "hooks": [{
                            "type": "command",
                            "command": "claude-plan-bridge writeback --event create --cwd '/old/path/b'"
                        }]
                    },
                    {
                        "matcher": "TaskUpdate",
                        "hooks": [{
                            "type": "command",
                            "command": "claude-plan-bridge writeback --event update --cwd '/old/path/a'"
                        }]
                    },
                    {
                        "matcher": "Edit",
                        "hooks": [{
                            "type": "command",
                            "command": "my-user-script /tmp/log"
                        }]
                    }
                ]
            }
        });
        std::fs::write(
            dir.join(".claude/settings.json"),
            serde_json::to_string_pretty(&dupes).unwrap(),
        )
        .unwrap();

        upgrade_hooks(&dir).unwrap();
        let raw = std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap();
        let settings: Value = serde_json::from_str(&raw).unwrap();

        // Exactly one plan-bridge UserPromptSubmit entry survives.
        let ups = settings
            .pointer("/hooks/UserPromptSubmit")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(
            ups.iter().filter(|e| is_plan_bridge_entry(e)).count(),
            1,
            "UserPromptSubmit must collapse to one plan-bridge entry: {ups:#?}"
        );

        // PostToolUse collapses to exactly two plan-bridge entries (TaskCreate
        // + TaskUpdate); the unrelated user hook is preserved.
        let post = settings
            .pointer("/hooks/PostToolUse")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(
            post.iter().filter(|e| is_plan_bridge_entry(e)).count(),
            2,
            "PostToolUse must collapse to two plan-bridge entries: {post:#?}"
        );
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
        assert!(has_user, "unrelated user hook must survive upgrade");

        // No stale path remains, and the result is drift-proof + portable.
        assert!(!raw.contains("/old/path/"), "stale paths survived: {raw}");
        assert!(hooks_have_drift_proof_cwd(&settings));
    }

    #[test]
    fn stale_baked_cwd_warnings_flags_dead_absolute_path() {
        // The bug that started Phase CA: a committed hook baking an absolute
        // path that no longer exists (here, the repo's pre-rename name).
        let dead = if cfg!(windows) {
            r"C:\Users\x\workspace\plan_to_task_bridge"
        } else {
            "/Users/x/workspace/plan_to_task_bridge"
        };
        let settings = json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": "TaskCreate",
                    "hooks": [{
                        "type": "command",
                        "command": format!("claude-plan-bridge writeback --event create --cwd '{dead}'")
                    }]
                }]
            }
        });
        let warns = stale_baked_cwd_warnings(&settings);
        assert_eq!(warns.len(), 1, "{warns:?}");
        assert!(warns[0].contains(dead), "{warns:?}");
        assert!(warns[0].contains("no longer exists"), "{warns:?}");
    }

    #[test]
    fn stale_baked_cwd_warnings_quiet_for_project_dir_form() {
        let settings = json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": "claude-plan-bridge reconcile --cwd \"$CLAUDE_PROJECT_DIR\""
                    }]
                }]
            }
        });
        assert!(stale_baked_cwd_warnings(&settings).is_empty());
    }

    #[test]
    fn stale_baked_cwd_warnings_flags_dir_without_plan() {
        // An absolute --cwd that exists but has no PLAN.md (the wrong
        // directory) is also a misconfiguration worth surfacing.
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let abs = dir.to_string_lossy().to_string();
        let settings = json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": format!("claude-plan-bridge reconcile --cwd '{abs}'")
                    }]
                }]
            }
        });
        let warns = stale_baked_cwd_warnings(&settings);
        assert_eq!(warns.len(), 1, "{warns:?}");
        assert!(warns[0].contains("no PLAN.md"), "{warns:?}");
    }

    #[test]
    fn hooks_have_drift_proof_cwd_rejects_relative() {
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
        assert!(!hooks_have_drift_proof_cwd(&settings));
    }

    #[test]
    fn hooks_have_drift_proof_cwd_accepts_project_dir() {
        // Phase CA: the portable form `init` / `upgrade-hooks` now emit. The
        // `$CLAUDE_PROJECT_DIR` reference is drift-proof even though it is not
        // a literal absolute path — Claude Code expands it to the project root
        // for every hook event.
        let settings = json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": "claude-plan-bridge reconcile --cwd \"$CLAUDE_PROJECT_DIR\""
                    }]
                }]
            }
        });
        assert!(hooks_have_drift_proof_cwd(&settings));
    }

    #[test]
    fn hooks_have_drift_proof_cwd_accepts_absolute() {
        // A legacy Phase-32 baked absolute path still resolves correctly on
        // the machine that wrote it, so it stays "drift-proof" — it's only the
        // cross-checkout portability that upgrade-hooks improves.
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
        assert!(hooks_have_drift_proof_cwd(&settings));
    }

    #[test]
    fn hooks_have_drift_proof_cwd_rejects_when_cwd_arg_relative() {
        // A bogus `--cwd .` shouldn't pass — a relative cwd is exactly what
        // drift-proofing eliminates.
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
        assert!(!hooks_have_drift_proof_cwd(&settings));
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

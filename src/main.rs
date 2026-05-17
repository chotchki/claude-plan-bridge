use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::io::Read;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "claude-plan-bridge",
    version,
    about = "Bridge PLAN.md to Claude Code's TaskCreate"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Shared scope flags for every project-aware subcommand. `--cwd` points at
/// the project directory (default `.`); `--plan` is an optional explicit
/// override of the PLAN.md path. When both are absent, the plan resolves to
/// `<cwd>/PLAN.md`. Backward compat: existing scripts that pass `--plan X.md`
/// keep working unchanged.
#[derive(Args, Clone)]
struct ProjectArgs {
    /// Project directory containing PLAN.md and the `.claude/` state dir.
    #[arg(long, default_value = ".")]
    cwd: PathBuf,
    /// Explicit PLAN.md path. Overrides `<cwd>/PLAN.md` when set.
    #[arg(long)]
    plan: Option<PathBuf>,
}

impl ProjectArgs {
    fn plan_path(&self) -> PathBuf {
        self.plan
            .clone()
            .unwrap_or_else(|| self.cwd.join("PLAN.md"))
    }
}

#[derive(Subcommand)]
enum Command {
    /// Parse a PLAN.md and emit the AST as JSON on stdout.
    Parse {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Apply a Claude Code PostToolUse hook event to PLAN.md.
    ///
    /// Reads the hook payload as JSON on stdin, writes any updates back to
    /// PLAN.md and the project state file, and emits a JSON hook response on
    /// stdout.
    Writeback {
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long, value_enum)]
        event: WritebackEvent,
    },
    /// Diff PLAN.md against the bridge's recorded state and emit any drift as
    /// `additionalContext` for Claude's next turn. Intended for the
    /// `UserPromptSubmit` hook; safe to run any time.
    Reconcile {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Sweep every fully-complete top-level phase from PLAN.md into
    /// PLAN_ARCHIVE.md (newest section at bottom), and drop the associated
    /// state mappings.
    Archive {
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        dry_run: bool,
        /// Date stamp for the archive section header (YYYY-MM-DD). Defaults
        /// to today (in UTC). Overridable for tests / reproducible builds.
        #[arg(long)]
        date: Option<String>,
    },
    /// Scaffold PLAN.md, install hooks into `.claude/settings.json`, and add
    /// `.claude/plan-bridge-state.json` to `.gitignore` for the project at
    /// `--cwd` (default: current directory). Idempotent.
    Init {
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        /// Overwrite an existing PLAN.md with the starter template.
        #[arg(long)]
        force: bool,
    },
    /// Run an MCP server over stdio that exposes plan-aware tools
    /// (`plan_list`, `plan_check`, `plan_uncheck`, `plan_add`, `plan_archive`).
    Serve {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Seed the state file with synthetic mappings for every leaf currently in
    /// PLAN.md so the first reconcile after install isn't a wall of
    /// `LeafAdded`. Idempotent. When Claude later TaskCreates against a
    /// baselined plan_path, the baseline mapping is silently replaced.
    Baseline {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Report PLAN.md / state file / hook health. First-stop diagnostic
    /// when TaskCreates appear to succeed but PLAN.md doesn't move.
    Status {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Emit a SessionStart additionalContext that drives Claude to rehydrate
    /// the in-session task list from the persisted state file. Intended for
    /// the `SessionStart` hook; safe to run any time (silent no-op when
    /// there's nothing to rehydrate).
    Resume {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Re-merge the latest plan-bridge hooks into an existing
    /// `.claude/settings.json` without touching PLAN.md or `.gitignore`. Use
    /// after upgrading the bridge binary on a project that was installed
    /// with an older version (e.g., one predating the SessionStart hook).
    /// Idempotent — safe to re-run.
    UpgradeHooks {
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
}

#[derive(Clone, ValueEnum)]
enum WritebackEvent {
    Create,
    Update,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Parse { project } => {
            let plan = project.plan_path();
            let input = std::fs::read_to_string(&plan)
                .with_context(|| format!("failed to read {}", plan.display()))?;
            let parsed = plan_bridge::parser::parse(&input)
                .with_context(|| format!("failed to parse {}", plan.display()))?;
            println!("{}", serde_json::to_string_pretty(&parsed)?);
        }
        Command::Writeback { project, event } => {
            let plan = project.plan_path();
            let output = run_writeback(&plan, event).unwrap_or_else(|e| {
                plan_bridge::hook::HookOutput::block(format!("claude-plan-bridge: {e:#}"))
            });
            let output = maybe_warn_missing_session_start(&project.cwd, output, "PostToolUse");
            println!("{}", output.to_json());
        }
        Command::Reconcile { project } => {
            let plan = project.plan_path();
            let output = run_reconcile(&plan).unwrap_or_else(|e| {
                plan_bridge::hook::HookOutput::block(format!("claude-plan-bridge: {e:#}"))
            });
            let output =
                maybe_warn_missing_session_start(&project.cwd, output, "UserPromptSubmit");
            println!("{}", output.to_json());
        }
        Command::Serve { project } => {
            plan_bridge::mcp::McpServer::new(project.plan_path()).serve()?;
        }
        Command::Baseline { project } => {
            let report = plan_bridge::baseline::baseline(&project.plan_path())?;
            println!(
                "claude-plan-bridge: baselined {} leaf(s), skipped {} already-mapped",
                report.baselined.len(),
                report.already_mapped.len()
            );
            if !report.skipped_no_id.is_empty() {
                println!(
                    "claude-plan-bridge: NOTE: skipped {} bare-checkbox leaf(s) with no id — \
                     untracked by the bridge (add an id like `1.2.3` to make them trackable):",
                    report.skipped_no_id.len()
                );
                for title in report.skipped_no_id.iter().take(5) {
                    let preview: String = title.chars().take(80).collect();
                    let trailer = if title.chars().count() > 80 {
                        "…"
                    } else {
                        ""
                    };
                    println!("    - {preview}{trailer}");
                }
                if report.skipped_no_id.len() > 5 {
                    println!("    ... (+{} more)", report.skipped_no_id.len() - 5);
                }
            }
        }
        Command::Init { cwd, force } => {
            let report = plan_bridge::init::init(&cwd, force)?;
            if report.created_plan {
                println!(
                    "claude-plan-bridge: created {}",
                    cwd.join("PLAN.md").display()
                );
            }
            if report.created_settings {
                println!(
                    "claude-plan-bridge: created {}",
                    cwd.join(".claude/settings.json").display()
                );
            } else if report.updated_settings {
                println!(
                    "claude-plan-bridge: merged hooks into {}",
                    cwd.join(".claude/settings.json").display()
                );
            }
            if report.created_gitignore {
                println!(
                    "claude-plan-bridge: created {}",
                    cwd.join(".gitignore").display()
                );
            } else if report.updated_gitignore {
                println!(
                    "claude-plan-bridge: appended state file to {}",
                    cwd.join(".gitignore").display()
                );
            }
            // Loud finale: Claude Code only loads .claude/settings.json at
            // session start, so hooks that init just wrote won't fire mid-
            // session. The recommended path is to init from a terminal
            // BEFORE opening Claude Code in the project. If you're already
            // in a session, the second block documents the recovery flow.
            eprintln!();
            eprintln!("▎ ✓ Recommended: run `claude-plan-bridge init` from a terminal BEFORE");
            eprintln!("▎   you open Claude Code in this project. The hooks load at session");
            eprintln!("▎   start and you skip the dance below entirely.");
            eprintln!();
            eprintln!("▎ ⚠ If you ran this from inside Claude Code: hooks are written but the");
            eprintln!("▎   session has settings cached. TaskCreates in this session will succeed");
            eprintln!("▎   in the harness but won't update PLAN.md. To recover, either:");
            eprintln!("▎     1. Restart Claude Code now — hooks fire for tasks created after.");
            eprintln!("▎     2. Keep working hand-edited; run `claude-plan-bridge baseline`");
            eprintln!("▎        later to seed state from your manually-maintained PLAN.md.");
            eprintln!();
        }
        Command::Status { project } => {
            run_status(&project)?;
        }
        Command::Resume { project } => {
            let plan = project.plan_path();
            let output = run_resume(&plan).unwrap_or_else(|e| {
                plan_bridge::hook::HookOutput::block(format!("claude-plan-bridge: {e:#}"))
            });
            println!("{}", output.to_json());
        }
        Command::UpgradeHooks { cwd } => {
            let report = plan_bridge::init::upgrade_hooks(&cwd)?;
            let settings = cwd.join(".claude/settings.json").display().to_string();
            if report.no_change {
                println!("claude-plan-bridge: {settings} already up to date");
            } else if report.created_settings {
                println!("claude-plan-bridge: created {settings} with plan-bridge hooks");
            } else if report.updated_settings {
                println!("claude-plan-bridge: merged latest plan-bridge hooks into {settings}");
            }
            eprintln!();
            eprintln!("▎ ⚠ Hooks reload only at Claude Code session start. If you're running");
            eprintln!("▎   `upgrade-hooks` from inside Claude Code, restart the session for the");
            eprintln!("▎   updated hook set (notably SessionStart, which rehydrates the task");
            eprintln!("▎   list from PLAN.md) to take effect.");
            eprintln!();
        }
        Command::Archive {
            project,
            dry_run,
            date,
        } => {
            let plan = project.plan_path();
            let date = date.unwrap_or_else(plan_bridge::today::today_utc);
            let report = plan_bridge::archive::archive(&plan, dry_run, &date)?;
            if report.is_empty() {
                println!("claude-plan-bridge: nothing to archive");
            } else {
                let verb = if report.dry_run {
                    "would archive"
                } else {
                    "archived"
                };
                println!(
                    "claude-plan-bridge: {verb} {} phase(s): {}",
                    report.archived_phase_ids.len(),
                    report.archived_phase_ids.join(", ")
                );
            }
        }
    }
    Ok(())
}

/// Read a hook payload from stdin and dispatch to the writeback handler. Any
/// error here surfaces to Claude as a `decision: "block"` hook response, never
/// as a stderr stack trace — the hook contract owns the channel.
fn run_writeback(
    plan: &std::path::Path,
    event: WritebackEvent,
) -> Result<plan_bridge::hook::HookOutput> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("read hook payload from stdin")?;
    let payload: plan_bridge::hook::HookPayload =
        serde_json::from_str(&buf).context("parse hook payload JSON")?;
    match event {
        WritebackEvent::Create => plan_bridge::writeback::writeback_create(&payload, plan),
        WritebackEvent::Update => plan_bridge::writeback::writeback_update(&payload, plan),
    }
}

fn run_status(project: &ProjectArgs) -> Result<()> {
    let plan_path = project.plan_path();
    let cwd = &project.cwd;
    let state_path = plan_bridge::state::default_state_path_for(&plan_path);
    let settings_path = cwd.join(".claude/settings.json");

    let mut all_good = true;

    // PLAN.md
    print!("PLAN.md: {}", plan_path.display());
    match std::fs::read_to_string(&plan_path) {
        Ok(text) => match plan_bridge::parser::parse(&text) {
            Ok(plan) => {
                let leaves = plan.leaves().len();
                let phases = plan.phases.len();
                let leaf_word = if leaves == 1 { "leaf" } else { "leaves" };
                let phase_word = if phases == 1 { "phase" } else { "phases" };
                println!(" ({leaves} {leaf_word} in {phases} top-level {phase_word})");
            }
            Err(e) => {
                all_good = false;
                println!(" — PARSE ERROR: {e}");
            }
        },
        Err(_) => {
            all_good = false;
            println!(" — MISSING");
        }
    }

    // State file
    print!("state file: {}", state_path.display());
    let state_exists = state_path.exists();
    if state_exists {
        match plan_bridge::state::State::load(&state_path) {
            Ok(state) => {
                let n = state.mappings.len();
                let mtime = std::fs::metadata(&state_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .map(format_relative_time)
                    .unwrap_or_else(|| "?".to_string());
                println!(" ({n} mappings, modified {mtime})");
            }
            Err(e) => {
                all_good = false;
                println!(" — LOAD ERROR: {e}");
            }
        }
    } else {
        println!(" — MISSING");
    }

    // Hooks in settings.json
    print!("hooks: {}", settings_path.display());
    let mut hooks_installed = false;
    if !settings_path.exists() {
        println!(" — MISSING");
    } else {
        match std::fs::read_to_string(&settings_path) {
            Ok(text) => {
                let count = text.matches("claude-plan-bridge").count();
                hooks_installed = count >= 3;
                if hooks_installed {
                    println!();
                    let want = [
                        ("SessionStart", "claude-plan-bridge resume"),
                        ("UserPromptSubmit", "claude-plan-bridge reconcile"),
                        (
                            "PostToolUse(TaskCreate)",
                            "claude-plan-bridge writeback --event create",
                        ),
                        (
                            "PostToolUse(TaskUpdate)",
                            "claude-plan-bridge writeback --event update",
                        ),
                    ];
                    for (label, cmd) in want {
                        let mark = if text.contains(cmd) { "✓" } else { "✗" };
                        if mark == "✗" {
                            all_good = false;
                        }
                        println!("  {mark} {label} → {cmd}");
                    }
                } else {
                    all_good = false;
                    println!(" — no claude-plan-bridge hooks found");
                }
            }
            Err(e) => {
                all_good = false;
                println!(" — READ ERROR: {e}");
            }
        }
    }

    // Binary version
    println!("binary: claude-plan-bridge {}", env!("CARGO_PKG_VERSION"));

    // Silent-failure detection: hooks installed but no state file.
    // The classic "init mid-session, hooks don't fire" symptom.
    if hooks_installed && !state_exists {
        println!();
        println!("⚠ Hooks are installed but the state file is missing — TaskCreate hooks");
        println!("  likely haven't fired. If you just ran `init` mid-session, restart");
        println!("  Claude Code so settings.json reloads. Subsequent TaskCreates will");
        println!("  then update PLAN.md as expected.");
        all_good = false;
    }

    if all_good {
        println!();
        println!("✓ all clear");
    }
    Ok(())
}

fn format_relative_time(t: std::time::SystemTime) -> String {
    let now = std::time::SystemTime::now();
    let secs = now
        .duration_since(t)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

fn run_reconcile(plan: &std::path::Path) -> Result<plan_bridge::hook::HookOutput> {
    let deltas = plan_bridge::reconcile::reconcile(plan)?;
    let rendered = plan_bridge::reconcile::render_deltas(&deltas);
    if rendered.is_empty() {
        Ok(plan_bridge::hook::HookOutput::silent())
    } else {
        Ok(plan_bridge::hook::HookOutput::context(
            "UserPromptSubmit",
            rendered,
        ))
    }
}

/// Splice a "SessionStart hook missing" warning onto the front of `output`
/// when the project lacks the SessionStart → resume entry. Keeps the real
/// hook payload intact; just prepends a yell so a user on a pre-25.2 install
/// can't ignore the drift. `hook_event` is the event we're responding to,
/// used only when `output` is silent (we need to label the new context).
fn maybe_warn_missing_session_start(
    cwd: &std::path::Path,
    output: plan_bridge::hook::HookOutput,
    hook_event: &str,
) -> plan_bridge::hook::HookOutput {
    match plan_bridge::init::missing_session_start_warning(cwd) {
        Some(warning) => output.prepend_context(hook_event, warning),
        None => output,
    }
}

fn run_resume(plan: &std::path::Path) -> Result<plan_bridge::hook::HookOutput> {
    match plan_bridge::resume::build_resume_message(plan)? {
        Some(msg) => Ok(plan_bridge::hook::HookOutput::context(
            "SessionStart",
            msg,
        )),
        None => Ok(plan_bridge::hook::HookOutput::silent()),
    }
}

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
            println!("{}", output.to_json());
        }
        Command::Reconcile { project } => {
            let plan = project.plan_path();
            let output = run_reconcile(&plan).unwrap_or_else(|e| {
                plan_bridge::hook::HookOutput::block(format!("claude-plan-bridge: {e:#}"))
            });
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

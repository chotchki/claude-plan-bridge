use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::io::Read;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "plan-bridge",
    version,
    about = "Bridge PLAN.md to Claude Code's TaskCreate"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse a PLAN.md and emit the AST as JSON on stdout.
    Parse {
        #[arg(long, default_value = "PLAN.md")]
        plan: PathBuf,
    },
    /// Apply a Claude Code PostToolUse hook event to PLAN.md.
    ///
    /// Reads the hook payload as JSON on stdin, writes any updates back to
    /// PLAN.md and the project state file, and emits a JSON hook response on
    /// stdout.
    Writeback {
        #[arg(long, default_value = "PLAN.md")]
        plan: PathBuf,
        #[arg(long, value_enum)]
        event: WritebackEvent,
    },
    /// Diff PLAN.md against the bridge's recorded state and emit any drift as
    /// `additionalContext` for Claude's next turn. Intended for the
    /// `UserPromptSubmit` hook; safe to run any time.
    Reconcile {
        #[arg(long, default_value = "PLAN.md")]
        plan: PathBuf,
    },
    /// Sweep every fully-complete top-level phase from PLAN.md into
    /// PLAN_ARCHIVE.md (newest section at top), and drop the associated state
    /// mappings.
    Archive {
        #[arg(long, default_value = "PLAN.md")]
        plan: PathBuf,
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
}

#[derive(Clone, ValueEnum)]
enum WritebackEvent {
    Create,
    Update,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Parse { plan } => {
            let input = std::fs::read_to_string(&plan)
                .with_context(|| format!("failed to read {}", plan.display()))?;
            let parsed = plan_bridge::parser::parse(&input)
                .with_context(|| format!("failed to parse {}", plan.display()))?;
            println!("{}", serde_json::to_string_pretty(&parsed)?);
        }
        Command::Writeback { plan, event } => {
            let output = run_writeback(&plan, event).unwrap_or_else(|e| {
                plan_bridge::hook::HookOutput::block(format!("plan-bridge: {e:#}"))
            });
            println!("{}", output.to_json());
        }
        Command::Reconcile { plan } => {
            let output = run_reconcile(&plan).unwrap_or_else(|e| {
                plan_bridge::hook::HookOutput::block(format!("plan-bridge: {e:#}"))
            });
            println!("{}", output.to_json());
        }
        Command::Init { cwd, force } => {
            let report = plan_bridge::init::init(&cwd, force)?;
            if report.created_plan {
                println!("plan-bridge: created {}", cwd.join("PLAN.md").display());
            }
            if report.created_settings {
                println!("plan-bridge: created {}", cwd.join(".claude/settings.json").display());
            } else if report.updated_settings {
                println!("plan-bridge: merged hooks into {}", cwd.join(".claude/settings.json").display());
            }
            if report.created_gitignore {
                println!("plan-bridge: created {}", cwd.join(".gitignore").display());
            } else if report.updated_gitignore {
                println!("plan-bridge: appended state file to {}", cwd.join(".gitignore").display());
            }
        }
        Command::Archive { plan, dry_run, date } => {
            let date = date.unwrap_or_else(today_utc);
            let report = plan_bridge::archive::archive(&plan, dry_run, &date)?;
            if report.is_empty() {
                println!("plan-bridge: nothing to archive");
            } else {
                let verb = if report.dry_run { "would archive" } else { "archived" };
                println!(
                    "plan-bridge: {verb} {} phase(s): {}",
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
        Ok(plan_bridge::hook::HookOutput::context(rendered))
    }
}

fn today_utc() -> String {
    // Avoid pulling in `chrono` just for a YYYY-MM-DD stamp. Compute from the
    // unix epoch via `SystemTime`.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = ymd_from_unix(secs);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Convert unix seconds (UTC) to (year, month, day). Civil-from-days based on
/// the Howard Hinnant algorithm — handles the proleptic Gregorian calendar
/// across the full 64-bit range.
fn ymd_from_unix(secs: u64) -> (i64, u32, u32) {
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

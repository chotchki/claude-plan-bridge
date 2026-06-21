use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::io::Read;
use std::path::{Path, PathBuf};

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
    /// Resolve the effective project root. Precedence:
    /// 1. An explicit, non-default `--cwd` (anything other than `.`/empty) —
    ///    honours operators who point the bridge at a specific directory.
    /// 2. `$CLAUDE_PROJECT_DIR`, the absolute project root Claude Code injects
    ///    into every hook, when set and non-empty. The installed hooks pass
    ///    `--cwd "$CLAUDE_PROJECT_DIR"`; this branch also rescues the rare
    ///    headless case where the shell expands that variable to empty.
    /// 3. The literal `--cwd` (`.`) as a last resort, resolved against the
    ///    subprocess cwd.
    fn root(&self) -> PathBuf {
        self.root_with_env(std::env::var_os("CLAUDE_PROJECT_DIR"))
    }

    /// Testable core of [`Self::root`] — `project_dir` is the value of
    /// `$CLAUDE_PROJECT_DIR` (passed in so tests don't mutate process env).
    fn root_with_env(&self, project_dir: Option<std::ffi::OsString>) -> PathBuf {
        let is_default = self.cwd.as_os_str().is_empty() || self.cwd == Path::new(".");
        if !is_default {
            return self.cwd.clone();
        }
        if let Some(dir) = project_dir
            && !dir.is_empty()
        {
            return PathBuf::from(dir);
        }
        self.cwd.clone()
    }

    fn plan_path(&self) -> PathBuf {
        self.plan
            .clone()
            .unwrap_or_else(|| self.root().join("PLAN.md"))
    }
}

#[derive(Subcommand)]
enum Command {
    /// Parse a PLAN.md and emit the AST as JSON on stdout.
    Parse {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Print the next phase id in the uppercase-letter sequence
    /// (`A`..`Z` -> `AA`..`AZ` -> `BA`..`BZ` -> ...), reconstructed by scanning
    /// PLAN.md and the sibling PLAN_ARCHIVE.md for the highest existing
    /// uppercase-letter phase id and incrementing it. Outputs `A` for a fresh
    /// project (legacy numeric phase ids are ignored). Scanning the archive too
    /// means a swept phase id is never re-handed-out. Call this before
    /// `TaskCreate`-ing a new phase so you don't have to hand-pick — or collide
    /// on — the next letter.
    NextPhase {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Apply a Claude Code PostToolUse hook event to PLAN.md.
    ///
    /// Reads the hook payload as JSON on stdin, writes any updates back to
    /// PLAN.md and the project state file, and emits a JSON hook response on
    /// stdout. `--dry-run` previews the effect: computes the mutation, writes
    /// a unified diff to stderr, and leaves PLAN.md + the state file
    /// untouched. Use for smoke-testing the bridge against an existing
    /// PLAN.md before installing hooks.
    Writeback {
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long, value_enum)]
        event: WritebackEvent,
        #[arg(long)]
        dry_run: bool,
    },
    /// Diff PLAN.md against the bridge's recorded state and emit any drift as
    /// `additionalContext` for Claude's next turn. Intended for the
    /// `UserPromptSubmit` hook; safe to run any time.
    Reconcile {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Archive top-level phases from PLAN.md into PLAN_ARCHIVE.md. With no
    /// `<phase>` argument: sweep every fully-complete phase (silent skip on
    /// any phase with pending leaves). With a `<phase>` argument: per-phase
    /// archive that errors loudly if the named phase has any `[ ]` Pending
    /// leaves. Pass `--descope-pending` to move pending leaves into the
    /// bottom `# Backlog (not yet phased)` section instead of erroring.
    Archive {
        #[command(flatten)]
        project: ProjectArgs,
        /// Phase id (e.g. `AI`, `1.0`). When provided, archives only that
        /// phase and errors on any unresolved leaves; omit for bulk sweep.
        phase: Option<String>,
        #[arg(long)]
        dry_run: bool,
        /// Date stamp for the archive section header (YYYY-MM-DD). Defaults
        /// to today (in UTC). Overridable for tests / reproducible builds.
        #[arg(long)]
        date: Option<String>,
        /// 38.5: when archiving a specific phase, move any `[ ]` Pending
        /// leaves to the bottom `# Backlog (not yet phased)` section first
        /// (as `- <id> - descoped from phase <PHASE> on <date>` bullets),
        /// then archive the now-fully-resolved phase. Errors are surfaced
        /// for any remaining pending non-leaf nodes the user has to resolve
        /// manually.
        #[arg(long)]
        descope_pending: bool,
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
    /// Toggle raw hook-payload capture for this project. When on, every
    /// writeback hook appends the verbatim stdin payload to
    /// `.claude/plan-bridge-debug.jsonl` — ground truth for diagnosing whether
    /// `metadata.plan_path` reaches the bridge. Persists in the state file;
    /// scoped to this project only. With no argument, prints the current state.
    Debug {
        #[command(flatten)]
        project: ProjectArgs,
        /// `on` / `off` (also accepts `true`/`false`, `1`/`0`). Omit to query.
        setting: Option<String>,
    },
    /// Release a stale state mapping without touching PLAN.md (BY.6). The
    /// recovery path for a mapping whose target leaf was hand-archived or
    /// hand-deleted, so its task id no longer points at a live line. The
    /// `archive` command already drops mappings for leaves it moves; use this
    /// when PLAN.md changed outside the bridge. `<target>` matches either the
    /// dotted leaf id (`BS.5`) or the raw task id (`baseline:BS.5`, `68`).
    /// Idempotent — a target with no matching mapping is a clean no-op.
    DropMapping {
        #[command(flatten)]
        project: ProjectArgs,
        /// plan_path (e.g. `BS.5`) or task id (e.g. `68`, `baseline:BS.5`).
        target: String,
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
    /// Defer a pending leaf: flip its checkbox to `[>]` (Backlog) and append
    /// a bullet under `## Backlog (not yet phased)` recording the source
    /// plan_path + date. Drops any state mapping pointing at this path.
    /// Archive treats Backlog like resolved, so the next phase exit sweeps
    /// the deferred leaf with the rest — the Backlog-section bullet survives
    /// to record the deferred work.
    Backlog {
        #[command(flatten)]
        project: ProjectArgs,
        /// Plan path to defer (e.g. `28.7`).
        plan_path: String,
        /// Override the date stamp (YYYY-MM-DD); defaults to today UTC.
        #[arg(long)]
        date: Option<String>,
    },
    /// Rewrite PLAN.md in canonical form: promote `### Phase N — Title`
    /// markdown headers to `- [ ] N.0` checkboxes, strip bold-wrapped IDs,
    /// normalize em-dash/hyphen separators. Routine writebacks no longer do
    /// this implicitly (Phase 29) — adopters with bespoke conventions keep
    /// their format until they explicitly invoke this subcommand. `--dry-run`
    /// reports what would change without writing.
    Canonicalize {
        #[command(flatten)]
        project: ProjectArgs,
        #[arg(long)]
        dry_run: bool,
    },
    /// Create a new FORMATv2 phase header (`## Phase <ID> - <title>`) with
    /// optional `*(depends on: ...)*` and `*(prefer after: ...)*` markers.
    /// For the common "just start typing tasks" path, TaskCreate's auto-anchor
    /// still works — `phase-add` is the surgical alternative when you want
    /// dependency metadata at creation time, or to pre-create an empty phase.
    PhaseAdd {
        #[command(flatten)]
        project: ProjectArgs,
        /// Phase id (typically alphabetic, e.g. `AI`, `AS`).
        id: String,
        /// Optional phase title (defaults to empty).
        title: Option<String>,
        /// Hard sequencing — comma-separated phase ids (`--depends-on AR,AQ`).
        #[arg(long, value_delimiter = ',')]
        depends_on: Vec<String>,
        /// Soft sequencing hint — comma-separated phase ids.
        #[arg(long, value_delimiter = ',')]
        prefer_after: Vec<String>,
        /// Insert immediately after this existing phase id (positional). Defaults
        /// to id-sort order.
        #[arg(long)]
        after: Option<String>,
    },
    /// Rename a phase. Phase-specific — refuses task ids to keep the
    /// operation explicit. For task renames, edit PLAN.md directly or use
    /// the `plan_rename` MCP tool.
    PhaseRename {
        #[command(flatten)]
        project: ProjectArgs,
        id: String,
        new_title: String,
    },
    /// Replace a phase's `depends_on` / `prefer_after` sequencing markers.
    /// At least one of `--depends-on` / `--prefer-after` must be passed.
    /// Pass an empty list (`--depends-on ""`) to clear; omit the flag to
    /// leave that side unchanged. Flips a legacy v1 anchor to FORMATv2 header
    /// form so the markers can be rendered.
    PhaseDeps {
        #[command(flatten)]
        project: ProjectArgs,
        id: String,
        #[arg(long, value_delimiter = ',')]
        depends_on: Option<Vec<String>>,
        #[arg(long, value_delimiter = ',')]
        prefer_after: Option<Vec<String>>,
    },
    /// Focus the bridge on one phase: subsequent SessionStart rehydration
    /// loads only that phase's leaves, reconcile foregrounds its drift,
    /// and writeback emits a soft warning on cross-phase TaskCreates.
    /// Persists in `.claude/plan-bridge-state.json` — survives /clear and
    /// outlives the Claude session. Surfaces any unmet `*(depends on)*`
    /// markers so sequencing constraints land up front.
    ///
    /// `plan_activate` is accepted as an alias so the CLI verb matches the
    /// `plan_activate` MCP tool name and the wording used in hook output /
    /// global CLAUDE.md (BY.4).
    #[command(visible_alias = "plan_activate")]
    Activate {
        #[command(flatten)]
        project: ProjectArgs,
        id: String,
    },
    /// Clear the active phase focus. After this, resume + reconcile +
    /// writeback behave as if activation had never been set. No-op when
    /// nothing was active.
    ///
    /// `plan_deactivate` is accepted as an alias to match the MCP tool name
    /// and the hook-output / CLAUDE.md wording (BY.4).
    #[command(visible_alias = "plan_deactivate")]
    Deactivate {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Phase 40.7: create a FORMATv2 phase header + N child tasks in a
    /// single atomic write. CLI-only convenience for scripting/scaffolding
    /// workflows from outside a Claude session (operator pre-seeds a phase
    /// before opening the session; reconcile surfaces the unmapped leaves
    /// on the agent's next prompt for normal TaskCreate mirroring).
    ///
    /// `--tasks "0:Lock,1:Audit,2:Driver"` is the leaf list. Each entry is
    /// `<id_suffix>:<subject>` — the bridge constructs `<PHASE>.<id_suffix>`
    /// for the leaf id. Subjects may contain colons (split is left-most
    /// only). Empty entries are skipped.
    ///
    /// Refuses to overwrite an existing phase — use `phase-add` for empty
    /// phase creation and individual `plan_add` (or TaskCreate from a
    /// session) for adding tasks to an existing phase.
    PhaseScaffold {
        #[command(flatten)]
        project: ProjectArgs,
        id: String,
        title: Option<String>,
        /// Comma-separated `id_suffix:subject` task definitions, e.g.
        /// `--tasks "0:Lock decisions,1:Audit,2:Driver build"`.
        #[arg(long, value_delimiter = ',')]
        tasks: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        depends_on: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        prefer_after: Vec<String>,
        /// Insert immediately after this existing phase id (positional).
        /// Defaults to id-sort order.
        #[arg(long)]
        after: Option<String>,
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
        Command::NextPhase { project } => {
            let plan = project.plan_path();
            println!("{}", plan_bridge::phase_seq::next_phase_id_for_plan(&plan));
        }
        Command::Writeback {
            project,
            event,
            dry_run,
        } => {
            let plan = project.plan_path();
            if dry_run {
                run_writeback_dry_run(&plan, event)?;
            } else {
                let output = plan_bridge::hook::guard_missing_plan(&plan, "PostToolUse", || {
                    run_writeback(&plan, event)
                });
                let output =
                    maybe_warn_missing_session_start(&project.root(), output, "PostToolUse");
                println!("{}", output.to_json());
            }
        }
        Command::Reconcile { project } => {
            let plan = project.plan_path();
            let output = plan_bridge::hook::guard_missing_plan(&plan, "UserPromptSubmit", || {
                run_reconcile(&plan)
            });
            let output =
                maybe_warn_missing_session_start(&project.root(), output, "UserPromptSubmit");
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
        Command::Debug { project, setting } => {
            let plan = project.plan_path();
            let state_path = plan_bridge::state::default_state_path_for(&plan);
            let mut state = plan_bridge::state::State::load(&state_path)?;
            let log_path = state_path.with_file_name("plan-bridge-debug.jsonl");
            match setting.as_deref() {
                None => {
                    let onoff = if state.debug { "on" } else { "off" };
                    println!("claude-plan-bridge: debug is {onoff}");
                    if state.debug {
                        println!("  capturing raw hook payloads to {}", log_path.display());
                    }
                }
                Some(s) => {
                    let want = match s.to_ascii_lowercase().as_str() {
                        "on" | "true" | "1" | "yes" => true,
                        "off" | "false" | "0" | "no" => false,
                        other => anyhow::bail!("debug: expected `on` or `off` (got `{other}`)"),
                    };
                    if state.debug == want {
                        let onoff = if want { "on" } else { "off" };
                        println!("claude-plan-bridge: debug already {onoff} (no-op)");
                    } else {
                        state.debug = want;
                        state.save(&state_path)?;
                        if want {
                            println!(
                                "claude-plan-bridge: debug ON — raw hook payloads will append to {}",
                                log_path.display()
                            );
                        } else {
                            println!(
                                "claude-plan-bridge: debug OFF — {} is no longer written (delete it to clean up)",
                                log_path.display()
                            );
                        }
                    }
                }
            }
        }
        Command::DropMapping { project, target } => {
            let plan = project.plan_path();
            let report = plan_bridge::drop_mapping::drop_mapping(&plan, &target)?;
            if report.dropped.is_empty() {
                println!(
                    "claude-plan-bridge: no mapping matched `{}` (no-op)",
                    report.target
                );
            } else {
                println!(
                    "claude-plan-bridge: dropped {} mapping(s) for `{}`: {}",
                    report.dropped.len(),
                    report.target,
                    report.dropped.join(", ")
                );
            }
        }
        Command::Resume { project } => {
            let plan = project.plan_path();
            let output =
                plan_bridge::hook::guard_missing_plan(&plan, "SessionStart", || run_resume(&plan));
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
        Command::Backlog {
            project,
            plan_path,
            date,
        } => {
            let plan = project.plan_path();
            let date = date.unwrap_or_else(plan_bridge::today::today_utc);
            let msg = plan_bridge::backlog::backlog(&plan, &plan_path, &date)?;
            println!("claude-plan-bridge: {msg}");
        }
        Command::Canonicalize { project, dry_run } => {
            let plan = project.plan_path();
            let report = plan_bridge::canonicalize::canonicalize(&plan, dry_run)?;
            let verb = match (report.dry_run, report.changed) {
                (true, true) => "would rewrite",
                (true, false) => "already canonical (dry-run)",
                (false, true) => "rewrote",
                (false, false) => "already canonical",
            };
            println!(
                "claude-plan-bridge: {verb} {} ({} header promotion(s))",
                plan.display(),
                report.notes.len()
            );
            for note in &report.notes {
                println!("  - {note}");
            }
        }
        Command::PhaseAdd {
            project,
            id,
            title,
            depends_on,
            prefer_after,
            after,
        } => {
            let plan_path = project.plan_path();
            let text = std::fs::read_to_string(&plan_path)
                .with_context(|| format!("read {}", plan_path.display()))?;
            let mut plan = plan_bridge::parser::parse(&text)?;
            if plan.find_phase(&id).is_some() {
                anyhow::bail!("phase `{id}` already exists in PLAN.md");
            }
            let title_str = title.unwrap_or_default();
            let new_phase = plan_bridge::ast::Phase::header_v2_with_deps(
                id.clone(),
                title_str.clone(),
                depends_on,
                prefer_after,
            );
            if let Some(after_id) = after {
                let pos = plan
                    .phases
                    .iter()
                    .position(|p| p.id == after_id)
                    .ok_or_else(|| {
                        anyhow::anyhow!("--after target `{after_id}` not found at top level")
                    })?;
                plan.phases.insert(pos + 1, new_phase);
            } else {
                plan.insert_phase(new_phase);
            }
            std::fs::write(&plan_path, plan_bridge::serializer::serialize(&plan))
                .with_context(|| format!("write {}", plan_path.display()))?;
            println!(
                "claude-plan-bridge: added phase `{id}` - `{title_str}` in {}",
                plan_path.display()
            );
        }
        Command::PhaseRename {
            project,
            id,
            new_title,
        } => {
            let plan_path = project.plan_path();
            let text = std::fs::read_to_string(&plan_path)
                .with_context(|| format!("read {}", plan_path.display()))?;
            let mut plan = plan_bridge::parser::parse(&text)?;
            let phase = plan
                .find_phase_mut(&id)
                .ok_or_else(|| anyhow::anyhow!("no phase with id `{id}` at top level"))?;
            if phase.title == new_title {
                println!("claude-plan-bridge: phase `{id}` already titled `{new_title}` (no-op)");
            } else {
                phase.title = new_title.clone();
                std::fs::write(&plan_path, plan_bridge::serializer::serialize(&plan))
                    .with_context(|| format!("write {}", plan_path.display()))?;
                println!("claude-plan-bridge: renamed phase `{id}` → `{new_title}`");
            }
        }
        Command::PhaseDeps {
            project,
            id,
            depends_on,
            prefer_after,
        } => {
            if depends_on.is_none() && prefer_after.is_none() {
                anyhow::bail!(
                    "phase-deps: pass at least one of `--depends-on` or `--prefer-after`"
                );
            }
            let plan_path = project.plan_path();
            let text = std::fs::read_to_string(&plan_path)
                .with_context(|| format!("read {}", plan_path.display()))?;
            let mut plan = plan_bridge::parser::parse(&text)?;
            let phase = plan
                .find_phase_mut(&id)
                .ok_or_else(|| anyhow::anyhow!("no phase with id `{id}` at top level"))?;
            phase.ensure_header_v2();
            if let Some(deps) = depends_on {
                phase.depends_on = deps.into_iter().filter(|s| !s.is_empty()).collect();
            }
            if let Some(after) = prefer_after {
                phase.prefer_after = after.into_iter().filter(|s| !s.is_empty()).collect();
            }
            let deps = phase.depends_on.clone();
            let after = phase.prefer_after.clone();
            std::fs::write(&plan_path, plan_bridge::serializer::serialize(&plan))
                .with_context(|| format!("write {}", plan_path.display()))?;
            println!(
                "claude-plan-bridge: updated deps for phase `{id}`: depends_on={deps:?}, prefer_after={after:?}"
            );
        }
        Command::Activate { project, id } => {
            let plan_path = project.plan_path();
            let text = std::fs::read_to_string(&plan_path)
                .with_context(|| format!("read {}", plan_path.display()))?;
            let plan = plan_bridge::parser::parse(&text)?;
            let phase = plan
                .find_phase(&id)
                .ok_or_else(|| anyhow::anyhow!("no phase with id `{id}` at top level"))?;
            // Compute unmet hard deps for the activation report.
            let active_ids: std::collections::HashSet<&str> =
                plan.phases.iter().map(|p| p.id.as_str()).collect();
            let unmet: Vec<&String> = phase
                .depends_on
                .iter()
                .filter(|d| active_ids.contains(d.as_str()))
                .collect();
            let state_path = plan_bridge::state::default_state_path_for(&plan_path);
            let mut state = plan_bridge::state::State::load(&state_path)?;
            let prior = state.active_phase().map(String::from);
            state.set_active_phase(Some(id.clone()));
            state.save(&state_path)?;
            match prior {
                Some(p) if p == id => {
                    println!("claude-plan-bridge: phase `{id}` already active (no-op)")
                }
                Some(p) => println!("claude-plan-bridge: activated phase `{id}` (was `{p}`)"),
                None => println!("claude-plan-bridge: activated phase `{id}`"),
            }
            if !unmet.is_empty() {
                let list = unmet
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                println!(
                    "  NOTE: depends on {list} — not yet archived (informational, not a gate)"
                );
            }
        }
        Command::Deactivate { project } => {
            let plan_path = project.plan_path();
            let state_path = plan_bridge::state::default_state_path_for(&plan_path);
            let mut state = plan_bridge::state::State::load(&state_path)?;
            match state.active_phase().map(String::from) {
                Some(prior) => {
                    state.set_active_phase(None);
                    state.save(&state_path)?;
                    println!("claude-plan-bridge: deactivated focus (was `{prior}`)");
                }
                None => {
                    println!("claude-plan-bridge: no active phase to deactivate (no-op)");
                }
            }
        }
        Command::PhaseScaffold {
            project,
            id,
            title,
            tasks,
            depends_on,
            prefer_after,
            after,
        } => {
            let plan_path = project.plan_path();
            let text = std::fs::read_to_string(&plan_path)
                .with_context(|| format!("read {}", plan_path.display()))?;
            let mut plan = plan_bridge::parser::parse(&text)?;
            if plan.find_phase(&id).is_some() {
                anyhow::bail!(
                    "phase `{id}` already exists — use `phase-add` for empty phases \
                     or add tasks individually with TaskCreate / plan_add"
                );
            }
            let title_str = title.unwrap_or_default();
            let children: Vec<plan_bridge::ast::Node> = tasks
                .iter()
                .filter_map(|spec| parse_task_spec(spec, &id))
                .collect();
            let new_phase = plan_bridge::ast::Phase {
                children,
                ..plan_bridge::ast::Phase::header_v2_with_deps(
                    id.clone(),
                    title_str.clone(),
                    depends_on,
                    prefer_after,
                )
            };
            let task_count = new_phase.children.len();
            if let Some(after_id) = after {
                let pos = plan
                    .phases
                    .iter()
                    .position(|p| p.id == after_id)
                    .ok_or_else(|| {
                        anyhow::anyhow!("--after target `{after_id}` not found at top level")
                    })?;
                plan.phases.insert(pos + 1, new_phase);
            } else {
                plan.insert_phase(new_phase);
            }
            std::fs::write(&plan_path, plan_bridge::serializer::serialize(&plan))
                .with_context(|| format!("write {}", plan_path.display()))?;
            println!(
                "claude-plan-bridge: scaffolded phase `{id}` - `{title_str}` with {task_count} task(s) in {}",
                plan_path.display()
            );
            println!(
                "  next: reconcile will surface the new leaves on the agent's next \
                 prompt for TaskCreate mirroring"
            );
        }
        Command::Archive {
            project,
            phase,
            dry_run,
            date,
            descope_pending,
        } => {
            let plan = project.plan_path();
            let date = date.unwrap_or_else(plan_bridge::today::today_utc);
            let report = match phase {
                Some(phase_id) => {
                    if dry_run {
                        anyhow::bail!("--dry-run is not yet supported for per-phase archive");
                    }
                    plan_bridge::archive::archive_phase(&plan, &phase_id, &date, descope_pending)?
                }
                None => {
                    if descope_pending {
                        anyhow::bail!(
                            "--descope-pending only applies to per-phase archive (provide a phase id)"
                        );
                    }
                    plan_bridge::archive::archive(&plan, dry_run, &date)?
                }
            };
            if report.is_empty() {
                println!("claude-plan-bridge: nothing to archive");
            } else {
                let verb = if report.dry_run {
                    "would archive"
                } else {
                    "archived"
                };
                // Report the total archived item count alongside the top-level
                // phase count: a single phase that bundles a dotted prefix
                // (e.g. `AE.0` carrying `AE.1`..`AE.11`) is one phase but many
                // items, and "1 phase" alone badly under-reports the sweep.
                println!(
                    "claude-plan-bridge: {verb} {} phase(s), {} item(s): {}",
                    report.archived_phase_ids.len(),
                    report.archived_plan_paths.len(),
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
    maybe_debug_dump(plan, &buf);
    let payload: plan_bridge::hook::HookPayload =
        serde_json::from_str(&buf).context("parse hook payload JSON")?;
    match event {
        WritebackEvent::Create => plan_bridge::writeback::writeback_create(&payload, plan),
        WritebackEvent::Update => plan_bridge::writeback::writeback_update(&payload, plan),
    }
}

/// Phase BY.11: when `debug` is enabled in the project state file, append the
/// raw hook payload (verbatim) to a sibling `plan-bridge-debug.jsonl` as one
/// JSON line `{"ts":<unix_secs>,"raw":<payload>}`. The capture is ground truth
/// for "what did the harness actually send?" — it records fields the bridge's
/// typed structs deliberately ignore, so it can confirm whether
/// `metadata.plan_path` arrived. Best-effort: any failure is swallowed so
/// debugging never breaks the hook. Off by default (state read returns
/// `debug=false`), so this is a no-op for every project that hasn't opted in.
fn maybe_debug_dump(plan: &std::path::Path, raw: &str) {
    let state_path = plan_bridge::state::default_state_path_for(plan);
    let debug_on = plan_bridge::state::State::load(&state_path)
        .map(|s| s.debug)
        .unwrap_or(false);
    if !debug_on {
        return;
    }
    let log_path = state_path.with_file_name("plan-bridge-debug.jsonl");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = writeln!(f, "{{\"ts\":{stamp},\"raw\":{}}}", raw.trim());
    }
}

/// Preview a writeback against PLAN.md without mutating disk. Copies the
/// real PLAN.md + state into a temp directory, runs the writeback there, and
/// emits a unified-diff-ish report of what would change. Useful for adopters
/// who want to smoke-test the bridge before installing hooks.
fn run_writeback_dry_run(plan: &std::path::Path, event: WritebackEvent) -> Result<()> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("read hook payload from stdin")?;
    let payload: plan_bridge::hook::HookPayload =
        serde_json::from_str(&buf).context("parse hook payload JSON")?;

    let tmp = tempdir_for_dry_run()?;
    let scratch_plan = tmp.join("PLAN.md");
    let scratch_claude = tmp.join(".claude");
    std::fs::create_dir_all(&scratch_claude).context("mkdir scratch .claude")?;
    let original =
        std::fs::read_to_string(plan).with_context(|| format!("read {}", plan.display()))?;
    std::fs::write(&scratch_plan, &original).context("seed scratch PLAN.md")?;
    let real_state_path = plan_bridge::state::default_state_path_for(plan);
    if real_state_path.exists() {
        let state_bytes =
            std::fs::read(&real_state_path).context("read real state file for dry-run")?;
        std::fs::write(scratch_claude.join("plan-bridge-state.json"), state_bytes)
            .context("seed scratch state")?;
    }

    let result = match event {
        WritebackEvent::Create => plan_bridge::writeback::writeback_create(&payload, &scratch_plan),
        WritebackEvent::Update => plan_bridge::writeback::writeback_update(&payload, &scratch_plan),
    };
    let after = std::fs::read_to_string(&scratch_plan).unwrap_or_else(|_| original.clone());
    print_dry_run_report(&original, &after, plan, result.as_ref().ok());
    if let Err(e) = result {
        eprintln!("\n[dry-run] writeback returned an error: {e:#}");
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

fn tempdir_for_dry_run() -> Result<std::path::PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = std::env::temp_dir().join(format!("plan-bridge-dryrun-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&p).context("mkdir dry-run scratch")?;
    Ok(p)
}

fn print_dry_run_report(
    before: &str,
    after: &str,
    plan_path: &std::path::Path,
    hook_output: Option<&plan_bridge::hook::HookOutput>,
) {
    eprintln!("[dry-run] target: {}", plan_path.display());
    if before == after {
        eprintln!("[dry-run] no change (writeback was a no-op)");
    } else {
        let added = after.lines().count() as isize - before.lines().count() as isize;
        eprintln!(
            "[dry-run] would change PLAN.md ({}{} line(s))",
            if added >= 0 { "+" } else { "" },
            added,
        );
        eprintln!("[dry-run] unified diff (lines only — not byte-perfect):");
        eprintln!("{}", line_diff(before, after));
    }
    if let Some(out) = hook_output {
        eprintln!("[dry-run] hook output (additionalContext only):");
        eprintln!("{}", out.to_json());
    }
}

/// Minimal unified-diff-ish renderer. Marks added lines with `+`, removed
/// with `-`, common context with two leading spaces. No diff hunks / line
/// numbers — adopters use this for "did the bridge do anything sketchy?",
/// not for `git apply`-grade patching.
fn line_diff(before: &str, after: &str) -> String {
    let mut out = String::new();
    let mut bi = before.lines().peekable();
    let mut ai = after.lines().peekable();
    while bi.peek().is_some() || ai.peek().is_some() {
        match (bi.peek(), ai.peek()) {
            (Some(b), Some(a)) if b == a => {
                out.push_str("  ");
                out.push_str(b);
                out.push('\n');
                bi.next();
                ai.next();
            }
            (Some(b), Some(a)) => {
                out.push_str("- ");
                out.push_str(b);
                out.push('\n');
                out.push_str("+ ");
                out.push_str(a);
                out.push('\n');
                bi.next();
                ai.next();
            }
            (Some(b), None) => {
                out.push_str("- ");
                out.push_str(b);
                out.push('\n');
                bi.next();
            }
            (None, Some(a)) => {
                out.push_str("+ ");
                out.push_str(a);
                out.push('\n');
                ai.next();
            }
            (None, None) => break,
        }
    }
    out
}

fn run_status(project: &ProjectArgs) -> Result<()> {
    let plan_path = project.plan_path();
    let root = project.root();
    let cwd = &root;
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
                    let parsed: Option<serde_json::Value> = serde_json::from_str(&text).ok();
                    let want: [(&str, &str); 4] = [
                        ("SessionStart", "resume"),
                        ("UserPromptSubmit", "reconcile"),
                        ("PostToolUse(TaskCreate)", "writeback"),
                        ("PostToolUse(TaskUpdate)", "writeback"),
                    ];
                    for (label, subcmd) in want {
                        // Map the friendly label back to the hook event name
                        // for the JSON walker.
                        let event = label.split('(').next().unwrap_or(label);
                        let present = parsed
                            .as_ref()
                            .map(|s| plan_bridge::init::hook_command_present(s, event, subcmd))
                            .unwrap_or(false);
                        let mark = if present { "✓" } else { "✗" };
                        if !present {
                            all_good = false;
                        }
                        println!("  {mark} {label} → claude-plan-bridge ... {subcmd}");
                    }
                    if let Some(s) = parsed.as_ref()
                        && !plan_bridge::init::hooks_have_drift_proof_cwd(s)
                    {
                        println!(
                            "  ⚠ hook entries are using a relative --cwd (or none) — \
                             run `claude-plan-bridge upgrade-hooks` to rewrite them to \
                             `--cwd \"$CLAUDE_PROJECT_DIR\"`, drift-proof and portable \
                             across checkouts."
                        );
                        all_good = false;
                    }
                    if let Some(s) = parsed.as_ref() {
                        let stale = plan_bridge::init::stale_baked_cwd_warnings(s);
                        if !stale.is_empty() {
                            for w in &stale {
                                println!("  ✗ {w}");
                            }
                            println!(
                                "    → this is the stale-path / renamed-checkout bug; run \
                                 `claude-plan-bridge upgrade-hooks` to switch to the portable \
                                 `--cwd \"$CLAUDE_PROJECT_DIR\"` form."
                            );
                            all_good = false;
                        }
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

/// Parse one `phase-scaffold --tasks` entry: `<id_suffix>:<subject>` →
/// `Some(Node { id: "<PHASE>.<id_suffix>", title: <subject>, ... })`.
/// Returns `None` for an empty spec (lets clap's value_delimiter pass
/// through trailing/leading whitespace cleanly). Subjects may contain
/// colons — split is left-most only.
fn parse_task_spec(spec: &str, phase_id: &str) -> Option<plan_bridge::ast::Node> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let (suffix, subject) = match spec.split_once(':') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (spec, ""), // bare suffix with no title — still emits
    };
    if suffix.is_empty() {
        return None;
    }
    Some(plan_bridge::ast::Node {
        id: format!("{phase_id}.{suffix}"),
        title: subject.to_string(),
        state: plan_bridge::ast::NodeState::Pending,
        id_style: plan_bridge::ast::IdStyle::Plain,
        separator: plan_bridge::ast::Separator::Hyphen,
        children: vec![],
        annotations: vec![],
    })
}

fn run_reconcile(plan: &std::path::Path) -> Result<plan_bridge::hook::HookOutput> {
    let deltas = plan_bridge::reconcile::reconcile(plan)?;
    // Phase 40.5: foreground active-phase drift when a focus is set.
    let state_path = plan_bridge::state::default_state_path_for(plan);
    let active_phase = plan_bridge::state::State::load(&state_path)
        .ok()
        .and_then(|s| s.active_phase.clone());
    let mut out = plan_bridge::reconcile::render_deltas_focused(&deltas, active_phase.as_deref());

    // Phase CD: append soft planning-loop nudges (auto-advance, working-set
    // hint, status-on-change heartbeat). Computed under the state lock because
    // they persist dedupe markers. Best-effort: any lock/state hiccup just
    // skips the nudges and keeps the drift report. Re-parsing the plan is cheap
    // for a per-prompt hook.
    if let Ok(text) = std::fs::read_to_string(plan)
        && let Ok(plan_ast) = plan_bridge::parser::parse(&text)
    {
        let nudges = plan_bridge::lock::with_state_lock(
            &state_path,
            plan_bridge::lock::DEFAULT_TIMEOUT,
            || {
                let mut state = plan_bridge::state::State::load(&state_path)?;
                let before = state.clone();
                let lines = plan_bridge::reconcile::planning_loop_context(&plan_ast, &mut state);
                if state != before {
                    state.save(&state_path)?;
                }
                Ok(lines)
            },
        )
        .unwrap_or_default();
        for n in nudges {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&n);
        }
    }

    if out.is_empty() {
        Ok(plan_bridge::hook::HookOutput::silent())
    } else {
        Ok(plan_bridge::hook::HookOutput::context(
            "UserPromptSubmit",
            out,
        ))
    }
}

/// Splice a "SessionStart hook missing" warning AND a "hook --cwd is
/// relative" warning onto the front of `output` when either condition is
/// detected. Keeps the real hook payload intact; just prepends a yell so a
/// user on a pre-25.2 (SessionStart) or pre-32 (relative --cwd) install can't
/// ignore the drift. `hook_event` is the event we're responding to, used
/// only when `output` is silent (we need to label the new context).
fn maybe_warn_missing_session_start(
    cwd: &std::path::Path,
    output: plan_bridge::hook::HookOutput,
    hook_event: &str,
) -> plan_bridge::hook::HookOutput {
    let output = match plan_bridge::init::outdated_hook_cwd_warning(cwd) {
        Some(warning) => output.prepend_context(hook_event, warning),
        None => output,
    };
    match plan_bridge::init::missing_session_start_warning(cwd) {
        Some(warning) => output.prepend_context(hook_event, warning),
        None => output,
    }
}

fn run_resume(plan: &std::path::Path) -> Result<plan_bridge::hook::HookOutput> {
    // SessionStart payload arrives on stdin with a `source` field
    // (startup/resume/clear/compact). On startup/clear the harness task list
    // is provably empty, so resume drops stale pending mappings before
    // emitting the prompt — avoiding harness-ID collisions when Claude's
    // fresh TaskCreates reuse low IDs. Stdin is best-effort: a missing or
    // unreadable payload is treated as an unknown source (no clearing).
    let source = {
        let mut buf = String::new();
        match std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf) {
            Ok(_) if !buf.trim().is_empty() => {
                serde_json::from_str::<plan_bridge::hook::HookPayload>(&buf)
                    .map(|p| p.source)
                    .unwrap_or_default()
            }
            _ => String::new(),
        }
    };
    match plan_bridge::resume::build_resume_message(plan, &source)? {
        Some(msg) => Ok(plan_bridge::hook::HookOutput::context("SessionStart", msg)),
        None => Ok(plan_bridge::hook::HookOutput::silent()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn args(cwd: &str, plan: Option<&str>) -> ProjectArgs {
        ProjectArgs {
            cwd: PathBuf::from(cwd),
            plan: plan.map(PathBuf::from),
        }
    }

    #[test]
    fn root_prefers_explicit_non_default_cwd_over_env() {
        // An operator who passes `--cwd /explicit` means it — even inside a
        // Claude session where $CLAUDE_PROJECT_DIR points elsewhere.
        let a = args("/explicit/project", None);
        let root = a.root_with_env(Some(OsString::from("/some/claude/project")));
        assert_eq!(root, PathBuf::from("/explicit/project"));
    }

    #[test]
    fn root_falls_back_to_claude_project_dir_when_cwd_is_default() {
        // The installed hooks pass `--cwd "$CLAUDE_PROJECT_DIR"`. If the shell
        // ever fails to expand it (so clap sees the default `.`), the env var
        // still rescues resolution to the real project root.
        let a = args(".", None);
        let root = a.root_with_env(Some(OsString::from("/claude/project/root")));
        assert_eq!(root, PathBuf::from("/claude/project/root"));
    }

    #[test]
    fn root_stays_literal_when_default_cwd_and_no_env() {
        // No env (e.g. a manual CLI run from the project root) → behave exactly
        // as before: resolve against the literal cwd.
        let a = args(".", None);
        assert_eq!(a.root_with_env(None), PathBuf::from("."));
    }

    #[test]
    fn root_ignores_empty_env() {
        // `$CLAUDE_PROJECT_DIR` set but empty (rare headless edge) must not
        // resolve to an empty path; fall through to the literal cwd.
        let a = args(".", None);
        assert_eq!(a.root_with_env(Some(OsString::new())), PathBuf::from("."));
    }

    #[test]
    fn plan_path_resolves_under_resolved_root() {
        // plan_path() builds on root(), so the env fallback flows through to
        // where the bridge looks for PLAN.md.
        let a = args(".", None);
        let resolved = a
            .root_with_env(Some(OsString::from("/claude/project")))
            .join("PLAN.md");
        assert_eq!(resolved, PathBuf::from("/claude/project/PLAN.md"));
        // An explicit --plan override is independent of root resolution.
        let b = args(".", Some("/custom/OTHER.md"));
        assert_eq!(b.plan_path(), PathBuf::from("/custom/OTHER.md"));
    }
}

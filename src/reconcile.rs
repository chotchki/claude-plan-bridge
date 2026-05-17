use crate::ast::{Node, NodeState, Plan};
use crate::parser::parse;
use crate::state::{State, default_state_path_for};
use crate::writeback::annotations_to_strings;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// One drift event between PLAN.md (canonical) and the bridge's last-known
/// state for a given task. Reconcile emits a `Vec<Delta>`; the renderer turns
/// that into a compact human-readable block for `additionalContext`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Delta {
    /// A leaf in PLAN.md that the bridge has no state mapping for. Claude
    /// should mirror it via `TaskCreate`.
    LeafAdded {
        plan_path: String,
        title: String,
        state: NodeState,
    },
    /// A leaf the bridge tracked is no longer in PLAN.md. Claude should call
    /// `TaskUpdate(status="deleted")` to mirror.
    LeafRemoved { plan_path: String, task_id: String },
    /// The leaf's checkbox state moved (e.g., `[ ]` → `[x]`, or `[x]` → `[-]`).
    /// The renderer maps each `(old, new)` pair to a human-readable suggestion
    /// (TaskUpdate completed / deleted / etc.).
    LeafStateChanged {
        plan_path: String,
        task_id: String,
        old: NodeState,
        new: NodeState,
    },
    /// Title text was edited in PLAN.md. Claude should `TaskUpdate(subject=...)`.
    LeafTitleChanged {
        plan_path: String,
        task_id: String,
        new_title: String,
        old_title: String,
    },
    /// Annotations under the leaf differ from last sync. The most common
    /// scenario: user added a note between turns and asked Claude to look.
    LeafAnnotationChanged {
        plan_path: String,
        task_id: String,
        new_annotations: Vec<String>,
    },
    /// A non-leaf node is `[x]` but its subtree still has unchecked leaves.
    /// Caught at reconcile time so the inconsistency surfaces before the
    /// archive sweep silently refuses to move the phase. Either the parent
    /// got ticked prematurely or its children's state lags behind reality.
    ParentInconsistent {
        plan_path: String,
        unchecked_descendants: Vec<String>,
    },
    /// State has `baseline:` mappings — leaves the bridge knows about but
    /// the harness's TaskList doesn't. Surfaces on every reconcile while
    /// baselines exist, so the agent can adopt them via
    /// `TaskCreate(metadata.plan_path=...)`. Resolves itself as adoptions
    /// evict the baseline mappings.
    BaselineOnly { plan_paths: Vec<String> },
}

/// Diff PLAN.md (current) against the bridge's recorded state. Emits one
/// `Delta` per drift event, in document order for PLAN-derived deltas
/// followed by `LeafRemoved` for orphaned state entries.
pub fn reconcile(plan_path: &Path) -> Result<Vec<Delta>> {
    let text = std::fs::read_to_string(plan_path)
        .with_context(|| format!("read {}", plan_path.display()))?;
    let plan = parse(&text).with_context(|| format!("parse {}", plan_path.display()))?;

    let state_path = default_state_path_for(plan_path);
    let state = State::load(&state_path)?;

    let path_to_task: HashMap<&str, &str> = state
        .mappings
        .iter()
        .map(|(tid, m)| (m.plan_path.as_str(), tid.as_str()))
        .collect();

    let mut deltas = Vec::new();
    let mut leaves = Vec::new();
    collect_leaves(&plan, &mut leaves);

    // All node ids (leaves AND parents) — used for the LeafRemoved check.
    // We need parents in the set so that a tracked node which grew children
    // (and thus stopped being a leaf) doesn't falsely register as removed.
    let mut all_node_paths: HashSet<String> = HashSet::new();
    for phase in &plan.phases {
        collect_all_paths(phase, &mut all_node_paths);
    }

    for leaf in leaves {
        // Phase 18: skip empty-id leaves (bare checkboxes without a dotted id).
        // They share plan_path="" which collapses in path_to_task — the bridge
        // can't tell them apart, so any diff emits noisy false-positive drift.
        // Untrackable by design; matched in baseline's skip rule.
        if leaf.id.is_empty() {
            continue;
        }
        match path_to_task.get(leaf.id.as_str()) {
            Some(&task_id) => {
                let mapping = state
                    .mappings
                    .get(task_id)
                    .expect("path_to_task built from state");
                if leaf.title != mapping.last_synced_title {
                    deltas.push(Delta::LeafTitleChanged {
                        plan_path: leaf.id.clone(),
                        task_id: task_id.to_string(),
                        new_title: leaf.title.clone(),
                        old_title: mapping.last_synced_title.clone(),
                    });
                }
                if leaf.state != mapping.last_synced_state {
                    deltas.push(Delta::LeafStateChanged {
                        plan_path: leaf.id.clone(),
                        task_id: task_id.to_string(),
                        old: mapping.last_synced_state,
                        new: leaf.state,
                    });
                }
                let current = annotations_to_strings(&leaf.annotations);
                if current != mapping.last_synced_annotations {
                    deltas.push(Delta::LeafAnnotationChanged {
                        plan_path: leaf.id.clone(),
                        task_id: task_id.to_string(),
                        new_annotations: current,
                    });
                }
            }
            None => {
                deltas.push(Delta::LeafAdded {
                    plan_path: leaf.id.clone(),
                    title: leaf.title.clone(),
                    state: leaf.state,
                });
            }
        }
    }

    // State entries whose plan_path no longer exists anywhere in PLAN.md → removed.
    // A tracked node that gained children is still present (just as a parent now),
    // so it stays in all_node_paths and does NOT trigger LeafRemoved.
    for (task_id, mapping) in &state.mappings {
        if !all_node_paths.contains(&mapping.plan_path) {
            deltas.push(Delta::LeafRemoved {
                plan_path: mapping.plan_path.clone(),
                task_id: task_id.clone(),
            });
        }
    }

    // Intra-PLAN.md sanity check: parent-checked-but-children-not.
    for phase in &plan.phases {
        collect_parent_inconsistencies(phase, &mut deltas);
    }

    // Phase 23 advisory: state.mappings whose task_id starts with `baseline:`
    // are leaves the bridge tracks but the harness's TaskList doesn't. Surface
    // them so the agent knows to adopt with TaskCreate.
    let baseline_paths: Vec<String> = state
        .mappings
        .iter()
        .filter(|(tid, _)| tid.starts_with(crate::baseline::BASELINE_PREFIX))
        .map(|(_, m)| m.plan_path.clone())
        .collect();
    if !baseline_paths.is_empty() {
        deltas.push(Delta::BaselineOnly {
            plan_paths: baseline_paths,
        });
    }

    Ok(deltas)
}

fn collect_parent_inconsistencies(node: &Node, out: &mut Vec<Delta>) {
    // A parent is "marked resolved" if it's [x] or [-]. If any descendant leaf
    // is still pending, we surface the inconsistency. WontDo counts as resolved
    // for both parent and descendants — a phase made of [-] leaves can
    // legitimately be archived.
    if node.is_resolved() && !node.is_leaf() {
        let mut unresolved: Vec<String> = Vec::new();
        collect_unresolved_descendants(node, &mut unresolved);
        if !unresolved.is_empty() {
            out.push(Delta::ParentInconsistent {
                plan_path: node.id.clone(),
                unchecked_descendants: unresolved,
            });
        }
    }
    for child in &node.children {
        collect_parent_inconsistencies(child, out);
    }
}

fn collect_unresolved_descendants(node: &Node, out: &mut Vec<String>) {
    for child in &node.children {
        if child.is_leaf() {
            if !child.is_resolved() {
                out.push(child.id.clone());
            }
        } else {
            collect_unresolved_descendants(child, out);
        }
    }
}

/// Render deltas into a compact human-readable block for Claude. Empty when
/// no deltas exist.
pub fn render_deltas(deltas: &[Delta]) -> String {
    if deltas.is_empty() {
        return String::new();
    }
    let mut out = String::from("PLAN.md drift since last sync:\n");
    for d in deltas {
        match d {
            Delta::LeafAdded {
                plan_path,
                title,
                state,
            } => {
                let mark = match state {
                    NodeState::Done => "[x]",
                    NodeState::WontDo => "[-]",
                    NodeState::Pending => "[ ]",
                };
                out.push_str(&format!(
                    "  + Added {mark} {plan_path} {title}  (consider TaskCreate)\n"
                ));
            }
            Delta::LeafRemoved { plan_path, task_id } => {
                out.push_str(&format!(
                    "  - Removed {plan_path}  (task {task_id} — consider TaskUpdate status=deleted)\n"
                ));
            }
            Delta::LeafStateChanged {
                plan_path,
                task_id,
                old,
                new,
            } => {
                let suggestion = match (old, new) {
                    (_, NodeState::Done) => "consider TaskUpdate status=completed",
                    (_, NodeState::WontDo) => {
                        "consider TaskUpdate status=deleted (the [-] line stays in PLAN.md)"
                    }
                    (NodeState::Done, NodeState::Pending) => {
                        "no TaskUpdate revives a completed task; informational"
                    }
                    (NodeState::WontDo, NodeState::Pending) => {
                        "task was previously skipped; consider TaskCreate to re-introduce"
                    }
                    _ => "informational",
                };
                out.push_str(&format!(
                    "  ~ State {plan_path} ({state_old:?} → {state_new:?}) (task {task_id} — {suggestion})\n",
                    state_old = old,
                    state_new = new,
                ));
            }
            Delta::LeafTitleChanged {
                plan_path,
                task_id,
                new_title,
                old_title,
            } => {
                out.push_str(&format!(
                    "  ~ Title {plan_path} (task {task_id})\n     was: {old_title}\n     now: {new_title}\n"
                ));
            }
            Delta::LeafAnnotationChanged {
                plan_path,
                task_id,
                new_annotations,
            } => {
                out.push_str(&format!(
                    "  + Annotations changed under {plan_path} (task {task_id})\n"
                ));
                for ann in new_annotations {
                    let preview: String = ann.lines().take(3).collect::<Vec<_>>().join(" / ");
                    // Char-aware truncation: `&s[..200]` slices BYTES, which
                    // panics if byte 200 falls inside a multi-byte UTF-8
                    // sequence (e.g. an em-dash `—` is 3 bytes). `.chars()`
                    // walks codepoints safely. Regression: a long bullet with
                    // an em-dash near byte 200 used to crash reconcile, which
                    // crashed every UserPromptSubmit since reconcile runs on
                    // every prompt.
                    let trimmed = if preview.chars().count() > 200 {
                        let mut s: String = preview.chars().take(200).collect();
                        s.push('…');
                        s
                    } else {
                        preview
                    };
                    // `annotation_to_string` serializes Annotation::Bullet as
                    // "- text", so the leading "- " is part of the payload.
                    // Strip it here so we don't render "- - text".
                    let body = trimmed.strip_prefix("- ").unwrap_or(&trimmed);
                    out.push_str(&format!("      - {body}\n"));
                }
            }
            Delta::ParentInconsistent {
                plan_path,
                unchecked_descendants,
            } => {
                out.push_str(&format!(
                    "  ! Inconsistent: {plan_path} is [x] but still has unchecked descendants ({}):\n",
                    unchecked_descendants.len()
                ));
                for u in unchecked_descendants {
                    out.push_str(&format!("      - {u}\n"));
                }
            }
            Delta::BaselineOnly { plan_paths } => {
                let preview: Vec<String> = plan_paths.iter().take(5).cloned().collect();
                let trailer = if plan_paths.len() > 5 {
                    format!(", +{} more", plan_paths.len() - 5)
                } else {
                    String::new()
                };
                out.push_str(&format!(
                    "  i {} leaf(s) tracked via baseline (not yet in TaskList): {}{}\n     Adopt with TaskCreate(metadata.plan_path=...) — writeback dedupes against existing lines.\n",
                    plan_paths.len(),
                    preview.join(", "),
                    trailer,
                ));
            }
        }
    }
    out
}

fn collect_leaves<'a>(plan: &'a Plan, out: &mut Vec<&'a Node>) {
    for phase in &plan.phases {
        collect_leaves_node(phase, out);
    }
}

fn collect_leaves_node<'a>(node: &'a Node, out: &mut Vec<&'a Node>) {
    if node.is_leaf() {
        out.push(node);
        return;
    }
    for child in &node.children {
        collect_leaves_node(child, out);
    }
}

fn collect_all_paths(node: &Node, out: &mut HashSet<String>) {
    out.insert(node.id.clone());
    for child in &node.children {
        collect_all_paths(child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Mapping, State, default_state_path_for};
    use std::path::PathBuf;

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plan-bridge-reconcile-{}-{}",
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

    fn write_plan(dir: &Path, contents: &str) -> PathBuf {
        let p = dir.join("PLAN.md");
        std::fs::write(&p, contents).unwrap();
        p
    }

    fn write_state(plan: &Path, mapping_pairs: &[(&str, Mapping)]) {
        let state_path = default_state_path_for(plan);
        let mut state = State::default();
        for (tid, m) in mapping_pairs {
            state.record(*tid, m.clone());
        }
        state.save(&state_path).unwrap();
    }

    fn mapping(plan_path: &str, title: &str, checked: bool, annotations: &[&str]) -> Mapping {
        Mapping {
            plan_path: plan_path.to_string(),
            last_synced_title: title.to_string(),
            last_synced_state: if checked {
                NodeState::Done
            } else {
                NodeState::Pending
            },
            last_synced_annotations: annotations.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn empty_plan_and_empty_state_yields_no_deltas() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "");
        let deltas = reconcile(&plan).unwrap();
        assert!(deltas.is_empty());
    }

    #[test]
    fn empty_id_leaves_are_skipped_silently() {
        // Phase 18.2 — quicksight shakeout. Bare-checkbox leaves (no dotted
        // id) share plan_path="" which collapses in the state lookup;
        // reconcile used to emit false LeafAdded / LeafTitleChanged for them
        // on every prompt. Now it skips them entirely — no drift, no noise.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 Real id\n  - [ ] First bare\n  - [ ] Second bare\n",
        );
        write_state(&plan, &[("t-1", mapping("1.1", "Real id", false, &[]))]);
        let deltas = reconcile(&plan).unwrap();
        // Only delta possible would be for 1.1; state matches PLAN.md so
        // we get NONE. Critically, no spam from the 2 empty-id leaves.
        assert!(
            deltas.is_empty(),
            "got false drift from empty-id leaves: {deltas:?}"
        );
    }

    #[test]
    fn no_drift_yields_no_deltas() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        write_state(&plan, &[("t-1", mapping("1.1", "Task", false, &[]))]);
        let deltas = reconcile(&plan).unwrap();
        assert!(deltas.is_empty(), "got: {deltas:?}");
    }

    #[test]
    fn emits_baseline_only_advisory_when_state_has_baseline_mappings() {
        // Phase 23.1 — third-project shakeout. On a fresh session against
        // a pre-populated PLAN.md, baseline-only mappings exist but TaskList
        // is empty. Reconcile should surface that count + plan_paths so the
        // agent knows to adopt via TaskCreate.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 First\n  - [ ] 1.2 Second\n",
        );
        write_state(
            &plan,
            &[
                ("baseline:1.1", mapping("1.1", "First", false, &[])),
                ("baseline:1.2", mapping("1.2", "Second", false, &[])),
            ],
        );
        let deltas = reconcile(&plan).unwrap();
        let found: Option<&Vec<String>> = deltas.iter().find_map(|d| match d {
            Delta::BaselineOnly { plan_paths } => Some(plan_paths),
            _ => None,
        });
        let paths = found.expect("BaselineOnly delta missing");
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"1.1".to_string()));
        assert!(paths.contains(&"1.2".to_string()));
    }

    #[test]
    fn baseline_only_silent_when_no_baseline_mappings() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        write_state(
            &plan,
            &[("real-task-id", mapping("1.1", "Task", false, &[]))],
        );
        let deltas = reconcile(&plan).unwrap();
        assert!(
            !deltas
                .iter()
                .any(|d| matches!(d, Delta::BaselineOnly { .. })),
            "no advisory when no baseline mappings"
        );
    }

    #[test]
    fn detects_leaf_added() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 First\n  - [ ] 1.2 Second\n",
        );
        write_state(&plan, &[("t-1", mapping("1.1", "First", false, &[]))]);
        let deltas = reconcile(&plan).unwrap();
        assert_eq!(deltas.len(), 1);
        assert!(matches!(
            &deltas[0],
            Delta::LeafAdded { plan_path, .. } if plan_path == "1.2"
        ));
    }

    #[test]
    fn tracked_node_that_became_a_parent_is_not_removed() {
        // Regression for Phase 9 bug: TaskCreate at 7.0 records a mapping; later
        // TaskCreates add 7.1, 7.2 as children. 7.0 stops being a leaf but is
        // still present in PLAN.md — reconcile must NOT fire LeafRemoved.
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 7.0 Parent that grew children\n  - [ ] 7.1 Child one\n  - [ ] 7.2 Child two\n",
        );
        write_state(
            &plan,
            &[(
                "t-parent",
                mapping("7.0", "Parent that grew children", false, &[]),
            )],
        );
        let deltas = reconcile(&plan).unwrap();
        assert!(
            !deltas.iter().any(|d| matches!(
                d,
                Delta::LeafRemoved { plan_path, .. } if plan_path == "7.0"
            )),
            "expected no LeafRemoved for parent-transitioned node, got: {deltas:?}"
        );
    }

    #[test]
    fn detects_leaf_removed() {
        let dir = scratch_dir();
        // 1.0 has a child so it isn't itself a leaf; only 1.1's absence drives the test.
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.2 Still here\n");
        write_state(
            &plan,
            &[
                ("t-stay", mapping("1.2", "Still here", false, &[])),
                ("t-1", mapping("1.1", "Gone", false, &[])),
            ],
        );
        let deltas = reconcile(&plan).unwrap();
        let removed: Vec<_> = deltas
            .iter()
            .filter_map(|d| match d {
                Delta::LeafRemoved { plan_path, task_id } => Some((plan_path, task_id)),
                _ => None,
            })
            .collect();
        assert_eq!(removed.len(), 1, "got: {deltas:?}");
        assert_eq!(removed[0], (&"1.1".to_string(), &"t-1".to_string()));
    }

    #[test]
    fn detects_leaf_checked() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [x] 1.1 Done\n");
        write_state(&plan, &[("t-1", mapping("1.1", "Done", false, &[]))]);
        let deltas = reconcile(&plan).unwrap();
        assert!(deltas.iter().any(|d| matches!(
            d,
            Delta::LeafStateChanged {
                new: NodeState::Done,
                ..
            }
        )));
    }

    #[test]
    fn detects_leaf_unchecked() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        write_state(&plan, &[("t-1", mapping("1.1", "Task", true, &[]))]);
        let deltas = reconcile(&plan).unwrap();
        assert!(deltas.iter().any(|d| matches!(
            d,
            Delta::LeafStateChanged {
                new: NodeState::Pending,
                old: NodeState::Done,
                ..
            }
        )));
    }

    #[test]
    fn detects_title_change() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Renamed task\n");
        write_state(&plan, &[("t-1", mapping("1.1", "Old name", false, &[]))]);
        let deltas = reconcile(&plan).unwrap();
        let found = deltas
            .iter()
            .find(|d| matches!(d, Delta::LeafTitleChanged { .. }));
        let Some(Delta::LeafTitleChanged {
            new_title,
            old_title,
            ..
        }) = found
        else {
            panic!("expected LeafTitleChanged, got {deltas:?}");
        };
        assert_eq!(new_title, "Renamed task");
        assert_eq!(old_title, "Old name");
    }

    #[test]
    fn detects_annotation_added() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n    This is a new note.\n",
        );
        write_state(&plan, &[("t-1", mapping("1.1", "Task", false, &[]))]);
        let deltas = reconcile(&plan).unwrap();
        let found = deltas
            .iter()
            .find(|d| matches!(d, Delta::LeafAnnotationChanged { .. }));
        let Some(Delta::LeafAnnotationChanged {
            new_annotations, ..
        }) = found
        else {
            panic!("expected LeafAnnotationChanged, got {deltas:?}");
        };
        assert_eq!(new_annotations.len(), 1);
        assert!(new_annotations[0].contains("new note"));
    }

    #[test]
    fn multiple_deltas_compound() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "\
- [ ] 1.0 Phase
  - [x] 1.1 Done
  - [ ] 1.2 Renamed
  - [ ] 1.3 Brand new
",
        );
        write_state(
            &plan,
            &[
                ("t-1", mapping("1.1", "Done", false, &[])), // checked diff
                ("t-2", mapping("1.2", "Old", false, &[])),  // title diff
                ("t-orphan", mapping("9.9", "Gone", false, &[])), // removed
            ],
        );
        let deltas = reconcile(&plan).unwrap();
        assert!(deltas.iter().any(|d| matches!(
            d,
            Delta::LeafStateChanged {
                new: NodeState::Done,
                ..
            }
        )));
        assert!(
            deltas
                .iter()
                .any(|d| matches!(d, Delta::LeafTitleChanged { .. }))
        );
        assert!(
            deltas
                .iter()
                .any(|d| matches!(d, Delta::LeafAdded { plan_path, .. } if plan_path == "1.3"))
        );
        assert!(
            deltas
                .iter()
                .any(|d| matches!(d, Delta::LeafRemoved { plan_path, .. } if plan_path == "9.9"))
        );
    }

    #[test]
    fn render_empty_yields_empty_string() {
        assert_eq!(render_deltas(&[]), "");
    }

    #[test]
    fn detects_parent_checked_but_child_unchecked() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "\
- [x] 1.0 Parent prematurely checked
  - [x] 1.1 Done
  - [ ] 1.2 Still pending
",
        );
        // State has both leaves mapped and consistent.
        write_state(
            &plan,
            &[
                ("t-1", mapping("1.1", "Done", true, &[])),
                ("t-2", mapping("1.2", "Still pending", false, &[])),
            ],
        );
        let deltas = reconcile(&plan).unwrap();
        let inconsistencies: Vec<_> = deltas
            .iter()
            .filter_map(|d| match d {
                Delta::ParentInconsistent {
                    plan_path,
                    unchecked_descendants,
                } => Some((plan_path, unchecked_descendants)),
                _ => None,
            })
            .collect();
        assert_eq!(inconsistencies.len(), 1, "got: {deltas:?}");
        assert_eq!(inconsistencies[0].0, "1.0");
        assert_eq!(inconsistencies[0].1, &vec!["1.2".to_string()]);
    }

    #[test]
    fn consistent_parent_emits_no_inconsistency_delta() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [x] 1.0 Parent\n  - [x] 1.1 Done\n  - [x] 1.2 Also done\n",
        );
        write_state(
            &plan,
            &[
                ("t-1", mapping("1.1", "Done", true, &[])),
                ("t-2", mapping("1.2", "Also done", true, &[])),
            ],
        );
        let deltas = reconcile(&plan).unwrap();
        assert!(
            !deltas
                .iter()
                .any(|d| matches!(d, Delta::ParentInconsistent { .. }))
        );
    }

    #[test]
    fn unchecked_parent_with_unchecked_child_emits_no_inconsistency() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Parent\n  - [ ] 1.1 Pending\n");
        write_state(&plan, &[("t-1", mapping("1.1", "Pending", false, &[]))]);
        let deltas = reconcile(&plan).unwrap();
        assert!(
            !deltas
                .iter()
                .any(|d| matches!(d, Delta::ParentInconsistent { .. }))
        );
    }

    #[test]
    fn deep_parent_inconsistency() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "\
- [x] 1.0 Phase
  - [x] 1.1 Task
    - [ ] 1.1.1 Sub
",
        );
        write_state(&plan, &[("t-1", mapping("1.1.1", "Sub", false, &[]))]);
        let deltas = reconcile(&plan).unwrap();
        let inconsistencies: Vec<_> = deltas
            .iter()
            .filter_map(|d| match d {
                Delta::ParentInconsistent {
                    plan_path,
                    unchecked_descendants,
                } => Some((plan_path.as_str(), unchecked_descendants.clone())),
                _ => None,
            })
            .collect();
        // Both 1.0 and 1.1 are inconsistent (each has 1.1.1 as unchecked descendant).
        assert_eq!(inconsistencies.len(), 2);
    }

    #[test]
    fn render_annotation_bullet_does_not_double_prefix() {
        // Regression for 7.6: when a bullet annotation reaches the renderer,
        // it arrives as "- text" (per annotation_to_string). Without the
        // strip_prefix, the output would read "      - - text".
        let deltas = vec![Delta::LeafAnnotationChanged {
            plan_path: "10.1".to_string(),
            task_id: "t-13".to_string(),
            new_annotations: vec!["- We'll go with an MIT license".to_string()],
        }];
        let r = render_deltas(&deltas);
        assert!(
            r.contains("      - We'll go with an MIT license"),
            "expected single bullet prefix, got: {r}"
        );
        assert!(!r.contains("- - We'll"), "double prefix leaked: {r}");
    }

    #[test]
    fn render_annotation_truncates_on_char_boundary_with_multibyte() {
        // Phase 16.1 regression — quicksight shakeout. A long annotation
        // containing a multi-byte char (e.g. em-dash `—`, 3 bytes UTF-8)
        // near the 200-byte boundary used to panic with
        //   `end byte index 200 is not a char boundary; it is inside '—'`
        // because the renderer did `&preview[..200]` (byte slice). Now uses
        // char-aware truncation; ensure no panic and ellipsis appended.
        //
        // Build a string whose byte 198..201 straddles an em-dash.
        let mut padded = "x".repeat(198); // 198 ASCII bytes = 198 chars
        padded.push('—'); // 3-byte UTF-8 char at bytes 198..201
        padded.push_str(&"x".repeat(50)); // plenty of tail so total chars > 200
        let deltas = vec![Delta::LeafAnnotationChanged {
            plan_path: "1.1".to_string(),
            task_id: "t-1".to_string(),
            new_annotations: vec![padded],
        }];
        let rendered = render_deltas(&deltas);
        assert!(rendered.contains('…'), "expected ellipsis on truncation");
        assert!(rendered.contains("1.1"), "expected plan_path in output");
    }

    #[test]
    fn render_annotation_text_keeps_single_prefix() {
        // Non-bullet annotations (Annotation::Text) don't have a leading "- ";
        // the renderer should still prepend exactly one "- ".
        let deltas = vec![Delta::LeafAnnotationChanged {
            plan_path: "1.1".to_string(),
            task_id: "t-1".to_string(),
            new_annotations: vec!["Plain text note".to_string()],
        }];
        let r = render_deltas(&deltas);
        assert!(r.contains("      - Plain text note"), "got: {r}");
    }

    #[test]
    fn render_non_empty_includes_each_delta() {
        let deltas = vec![
            Delta::LeafAdded {
                plan_path: "1.3".to_string(),
                title: "New".to_string(),
                state: NodeState::Pending,
            },
            Delta::LeafStateChanged {
                plan_path: "1.1".to_string(),
                task_id: "t-1".to_string(),
                old: NodeState::Pending,
                new: NodeState::Done,
            },
        ];
        let r = render_deltas(&deltas);
        assert!(r.contains("Added"));
        assert!(r.contains("1.3"));
        assert!(r.contains("State"));
        assert!(r.contains("1.1"));
    }
}

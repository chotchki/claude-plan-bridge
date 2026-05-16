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

    let mut seen_paths: HashSet<String> = HashSet::new();
    for leaf in leaves {
        seen_paths.insert(leaf.id.clone());
        match path_to_task.get(leaf.id.as_str()) {
            Some(&task_id) => {
                let mapping = state.mappings.get(task_id).expect("path_to_task built from state");
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

    // State entries whose plan_path no longer exists in PLAN.md → removed.
    for (task_id, mapping) in &state.mappings {
        if !seen_paths.contains(&mapping.plan_path) {
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
            Delta::LeafAdded { plan_path, title, state } => {
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
            Delta::LeafStateChanged { plan_path, task_id, old, new } => {
                let suggestion = match (old, new) {
                    (_, NodeState::Done) => "consider TaskUpdate status=completed",
                    (_, NodeState::WontDo) => "consider TaskUpdate status=deleted (the [-] line stays in PLAN.md)",
                    (NodeState::Done, NodeState::Pending) => "no TaskUpdate revives a completed task; informational",
                    (NodeState::WontDo, NodeState::Pending) => "task was previously skipped; consider TaskCreate to re-introduce",
                    _ => "informational",
                };
                out.push_str(&format!(
                    "  ~ State {plan_path} ({state_old:?} → {state_new:?}) (task {task_id} — {suggestion})\n",
                    state_old = old,
                    state_new = new,
                ));
            }
            Delta::LeafTitleChanged { plan_path, task_id, new_title, old_title } => {
                out.push_str(&format!(
                    "  ~ Title {plan_path} (task {task_id})\n     was: {old_title}\n     now: {new_title}\n"
                ));
            }
            Delta::LeafAnnotationChanged { plan_path, task_id, new_annotations } => {
                out.push_str(&format!(
                    "  + Annotations changed under {plan_path} (task {task_id})\n"
                ));
                for ann in new_annotations {
                    let preview: String = ann.lines().take(3).collect::<Vec<_>>().join(" / ");
                    let trimmed = if preview.len() > 200 {
                        format!("{}…", &preview[..200])
                    } else {
                        preview
                    };
                    out.push_str(&format!("      - {trimmed}\n"));
                }
            }
            Delta::ParentInconsistent { plan_path, unchecked_descendants } => {
                out.push_str(&format!(
                    "  ! Inconsistent: {plan_path} is [x] but still has unchecked descendants ({}):\n",
                    unchecked_descendants.len()
                ));
                for u in unchecked_descendants {
                    out.push_str(&format!("      - {u}\n"));
                }
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
            last_synced_state: if checked { NodeState::Done } else { NodeState::Pending },
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
    fn no_drift_yields_no_deltas() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        write_state(&plan, &[("t-1", mapping("1.1", "Task", false, &[]))]);
        let deltas = reconcile(&plan).unwrap();
        assert!(deltas.is_empty(), "got: {deltas:?}");
    }

    #[test]
    fn detects_leaf_added() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 First\n  - [ ] 1.2 Second\n");
        write_state(&plan, &[("t-1", mapping("1.1", "First", false, &[]))]);
        let deltas = reconcile(&plan).unwrap();
        assert_eq!(deltas.len(), 1);
        assert!(matches!(
            &deltas[0],
            Delta::LeafAdded { plan_path, .. } if plan_path == "1.2"
        ));
    }

    #[test]
    fn detects_leaf_removed() {
        let dir = scratch_dir();
        // 1.0 has a child so it isn't itself a leaf; only 1.1's absence drives the test.
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Phase\n  - [ ] 1.2 Still here\n",
        );
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
            Delta::LeafStateChanged { new: NodeState::Done, .. }
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
            Delta::LeafStateChanged { new: NodeState::Pending, old: NodeState::Done, .. }
        )));
    }

    #[test]
    fn detects_title_change() {
        let dir = scratch_dir();
        let plan = write_plan(&dir, "- [ ] 1.0 Phase\n  - [ ] 1.1 Renamed task\n");
        write_state(&plan, &[("t-1", mapping("1.1", "Old name", false, &[]))]);
        let deltas = reconcile(&plan).unwrap();
        let found = deltas.iter().find(|d| matches!(d, Delta::LeafTitleChanged { .. }));
        let Some(Delta::LeafTitleChanged { new_title, old_title, .. }) = found else {
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
        let Some(Delta::LeafAnnotationChanged { new_annotations, .. }) = found else {
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
        assert!(deltas.iter().any(|d| matches!(d, Delta::LeafStateChanged { new: NodeState::Done, .. })));
        assert!(deltas.iter().any(|d| matches!(d, Delta::LeafTitleChanged { .. })));
        assert!(deltas.iter().any(|d| matches!(d, Delta::LeafAdded { plan_path, .. } if plan_path == "1.3")));
        assert!(deltas.iter().any(|d| matches!(d, Delta::LeafRemoved { plan_path, .. } if plan_path == "9.9")));
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
                Delta::ParentInconsistent { plan_path, unchecked_descendants } => {
                    Some((plan_path, unchecked_descendants))
                }
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
        assert!(!deltas.iter().any(|d| matches!(d, Delta::ParentInconsistent { .. })));
    }

    #[test]
    fn unchecked_parent_with_unchecked_child_emits_no_inconsistency() {
        let dir = scratch_dir();
        let plan = write_plan(
            &dir,
            "- [ ] 1.0 Parent\n  - [ ] 1.1 Pending\n",
        );
        write_state(&plan, &[("t-1", mapping("1.1", "Pending", false, &[]))]);
        let deltas = reconcile(&plan).unwrap();
        assert!(!deltas.iter().any(|d| matches!(d, Delta::ParentInconsistent { .. })));
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
                Delta::ParentInconsistent { plan_path, unchecked_descendants } => {
                    Some((plan_path.as_str(), unchecked_descendants.clone()))
                }
                _ => None,
            })
            .collect();
        // Both 1.0 and 1.1 are inconsistent (each has 1.1.1 as unchecked descendant).
        assert_eq!(inconsistencies.len(), 2);
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

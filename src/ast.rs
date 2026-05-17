use serde::{Deserialize, Serialize};

/// A parsed PLAN.md document.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// Lines preceding the first checkbox node, preserved verbatim for round-trip.
    pub preamble: Vec<String>,
    /// Top-level nodes (phases). A node is a "leaf" when its `children` vec is empty.
    pub phases: Vec<Node>,
}

/// A single checkbox node in the plan. Phases, tasks, and subtasks all share this shape;
/// depth is determined by the dotted `id` (e.g., `1.0`, `1.1`, `1.1.1`) and by tree position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub state: NodeState,
    pub children: Vec<Node>,
    pub annotations: Vec<Annotation>,
}

/// Checkbox state. `Pending` = `[ ]`, `Done` = `[x]`, `WontDo` = `[-]`.
///
/// `Done` and `WontDo` are both "resolved" — archive treats them
/// equivalently — but they're semantically distinct in PLAN.md: `WontDo`
/// captures *we decided not to do this*, which is information worth keeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    #[default]
    Pending,
    Done,
    WontDo,
}

impl NodeState {
    /// True when this leaf is no longer active work — either done or
    /// explicitly skipped. Archive uses this; reconcile draws a finer line.
    pub fn is_resolved(self) -> bool {
        matches!(self, NodeState::Done | NodeState::WontDo)
    }
}

impl Node {
    pub fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }

    pub fn is_done(&self) -> bool {
        self.state == NodeState::Done
    }

    /// Collect every leaf in this subtree (depth-first, document order).
    pub fn leaves(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        collect_leaves(self, &mut out);
        out
    }

    pub fn is_resolved(&self) -> bool {
        self.state.is_resolved()
    }

    /// Recursively search this subtree for a node whose `id` matches.
    pub fn find(&self, id: &str) -> Option<&Node> {
        if self.id == id {
            return Some(self);
        }
        for child in &self.children {
            if let Some(n) = child.find(id) {
                return Some(n);
            }
        }
        None
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut Node> {
        if self.id == id {
            return Some(self);
        }
        for child in &mut self.children {
            if let Some(n) = child.find_mut(id) {
                return Some(n);
            }
        }
        None
    }
}

fn collect_leaves<'a>(node: &'a Node, out: &mut Vec<&'a Node>) {
    if node.is_leaf() {
        out.push(node);
        return;
    }
    for child in &node.children {
        collect_leaves(child, out);
    }
}

pub fn looks_like_markdown_header(text: &str) -> bool {
    let trimmed = text.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && matches!(trimmed.chars().nth(hashes), Some(' '))
}

/// Parse `### Phase N — Title` style headers OR the more general
/// `### <id> — Title` style. Returns `(id, title)` when the header matches;
/// None otherwise (caller treats as unrecognized).
///
/// Accepts numeric and alphanumeric id tokens. Dotted ids preserved verbatim
/// (`Phase 3.5` → id `3.5`, `### AA.A — ...` → id `AA.A`). Pure numeric or
/// pure-alpha ids get `.0` appended (`Phase 1` → `1.0`, `### AA — ...` →
/// `AA.0`) so parent_id_for of children resolves correctly. The general path
/// REQUIRES an em-dash or hyphen separator after the id, to keep generic
/// headings (`### Architecture`, `## Notes`) from being mistakenly promoted.
fn parse_phase_header(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    // Promotion only fires for `##` and `###`. `#` is too shallow to be a
    // phase header; `####+` is sub-section labeling inside a phase (the real
    // hierarchy lives in dotted ids, e.g. `X.4.a.1`). Both stay as narrative
    // annotations on serialize (preserved at original indent — see
    // write_annotation).
    if !(2..=3).contains(&hashes) {
        return None;
    }
    let after_hashes = trimmed.get(hashes..)?.trim_start();

    // Legacy: `Phase N — Title`. Strip the `Phase ` keyword and recurse.
    if let Some(after_phase) = after_hashes.strip_prefix("Phase ") {
        return parse_id_with_separator(after_phase);
    }

    // General: `<id> — Title` with a required em-dash/hyphen separator.
    parse_id_with_separator(after_hashes)
}

fn parse_id_with_separator(s: &str) -> Option<(String, String)> {
    let s = s.trim_start();
    let id_end = s
        .find(|c: char| c.is_whitespace() || c == '—' || c == '-')
        .unwrap_or(s.len());
    let id_part = &s[..id_end];
    if id_part.is_empty() {
        return None;
    }
    let mut chars = id_part.chars();
    if !chars.next()?.is_ascii_alphanumeric() {
        return None;
    }
    if !id_part
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.')
    {
        return None;
    }

    // Require em-dash or hyphen separator after the id, NOT just whitespace —
    // that's the guard that keeps generic `### Architecture` from being
    // mistaken for an `### Architecture — ...` phase heading.
    let after_id = s[id_end..].trim_start_matches(|c: char| c.is_whitespace());
    if !(after_id.starts_with('—') || after_id.starts_with('-')) {
        return None;
    }

    let id = if id_part.contains('.') {
        id_part.to_string()
    } else {
        format!("{id_part}.0")
    };
    let title = after_id
        .trim_start_matches('—')
        .trim_start_matches('-')
        .trim()
        .to_string();
    Some((id, title))
}

/// Depth-first: strip every `Phase N — Title` header annotation from this
/// node and all descendants. Captures (id, title) into `out` in document
/// order (descendants first, then this node's own annotations).
fn strip_and_collect_phase_headers(
    node: &mut Node,
    out: &mut Vec<(String, String)>,
    conversions: &mut Vec<String>,
) {
    for child in &mut node.children {
        strip_and_collect_phase_headers(child, out, conversions);
    }
    node.annotations.retain(|a| {
        if let Annotation::Text { text, .. } = a
            && let Some((id, title)) = parse_phase_header(text)
        {
            let preview = text.lines().next().unwrap_or("").trim().to_string();
            conversions.push(format!("{preview} → `- [ ] {id} {title}`"));
            out.push((id, title));
            false
        } else {
            true
        }
    });
}

fn flush_phase_group(
    out: &mut Vec<Node>,
    pending: &mut Vec<Node>,
    current_header: &mut Option<(String, String)>,
) {
    if pending.is_empty() {
        return;
    }
    if let Some((id, title)) = current_header.take() {
        let children: Vec<Node> = std::mem::take(pending);
        out.push(Node {
            id,
            title,
            state: NodeState::Pending,
            children,
            annotations: vec![],
        });
    } else {
        out.append(pending);
    }
}

impl Plan {
    /// Every leaf across all phases. Document order.
    pub fn leaves(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        for phase in &self.phases {
            collect_leaves(phase, &mut out);
        }
        out
    }

    /// Full-tree search by id. O(N); plans are small.
    pub fn find(&self, id: &str) -> Option<&Node> {
        for p in &self.phases {
            if let Some(n) = p.find(id) {
                return Some(n);
            }
        }
        None
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut Node> {
        for p in &mut self.phases {
            if let Some(n) = p.find_mut(id) {
                return Some(n);
            }
        }
        None
    }

    /// Insert a child into the node with `parent_id`, positioned in id-sort
    /// order against its siblings. Lets `1.2a` land between `1.2` and `1.3`
    /// without renumbering. Returns Err if no such parent.
    pub fn add_child_of(&mut self, parent_id: &str, child: Node) -> Result<(), String> {
        let parent = self
            .find_mut(parent_id)
            .ok_or_else(|| format!("no node with id {parent_id} in PLAN.md"))?;
        insert_in_order(&mut parent.children, child);
        Ok(())
    }

    /// Insert a top-level phase in id-sort order against existing phases.
    pub fn insert_phase(&mut self, phase: Node) {
        insert_in_order(&mut self.phases, phase);
    }

    /// Standardize a plan to canonical form before writeback. Promotes
    /// `### Phase N — Title` markdown headers (which the parser captures as
    /// annotations) into proper `N.0` phase nodes, with subsequent top-level
    /// phases re-parented as children. Returns the rewritten plan plus a list
    /// of human-readable conversion notes for the hook output so the user
    /// sees what changed.
    ///
    /// Refuses with Err when a header doesn't match the `Phase N — Title`
    /// pattern (e.g., `## Notes`, `### Phase 2/3 — ...`) — those need manual
    /// resolution. Phase numbers with dots (`Phase 3.5`) are accepted and used
    /// verbatim as the id (so `Phase 3.5` becomes `3.5`, not `3.5.0`).
    pub fn standardize_to_canonical(self) -> Result<(Plan, Vec<String>), String> {
        // No refusal pass — headers that don't match the promotion shape
        // stay as `Annotation::Text` and get emitted verbatim at their
        // original indent by the serializer. Narrative dividers like
        // `## Phase history`, `### Parallelism map`, or `#### X.4.a` are
        // preserved in-place; only `##` / `###` headers matching
        // `<id> — Title` get promoted to canonical phase checkboxes.

        // For each top-level phase, depth-first strip every
        // Phase-N header annotation from anywhere in its subtree (headers
        // attached to nested leaves count too — the parser attaches a header
        // to whichever node was open at that indent level). Captured in
        // document order.
        let mut tagged: Vec<(Vec<(String, String)>, Node)> = Vec::new();
        let mut conversions: Vec<String> = Vec::new();
        for mut phase in self.phases {
            let mut headers_in_subtree: Vec<(String, String)> = Vec::new();
            strip_and_collect_phase_headers(&mut phase, &mut headers_in_subtree, &mut conversions);
            tagged.push((headers_in_subtree, phase));
        }

        // Third pass: each phase's outgoing header is the LAST one in its
        // subtree (in document order). Multiple headers in one phase's content
        // would mean the user nested `### Phase N` markers *inside* a phase —
        // ambiguous; refuse rather than guess.
        let mut new_phases: Vec<Node> = Vec::new();
        let mut pending: Vec<Node> = Vec::new();
        let mut current_header: Option<(String, String)> = None;
        for (mut headers, phase) in tagged {
            if headers.len() > 1 {
                return Err(format!(
                    "phase `{}` has {} `### Phase N — Title` headers within its content — \
                     ambiguous (which one ends the phase?). Re-organize so each Phase header \
                     sits between top-level phase blocks, not nested inside one.",
                    phase.id,
                    headers.len()
                ));
            }
            pending.push(phase);
            if let Some((id, title)) = headers.pop() {
                flush_phase_group(&mut new_phases, &mut pending, &mut current_header);
                current_header = Some((id, title));
            }
        }
        flush_phase_group(&mut new_phases, &mut pending, &mut current_header);

        Ok((
            Plan {
                preamble: self.preamble,
                phases: new_phases,
            },
            conversions,
        ))
    }

    /// Remove a node by id from anywhere in the tree. Returns the detached
    /// node when found. Does not cascade-remove orphaned empty parents
    /// (deliberate v1 decision per PLAN.md 2.3.3).
    pub fn remove(&mut self, id: &str) -> Option<Node> {
        if let Some(idx) = self.phases.iter().position(|n| n.id == id) {
            return Some(self.phases.remove(idx));
        }
        for phase in &mut self.phases {
            if let Some(detached) = remove_descendant(phase, id) {
                return Some(detached);
            }
        }
        None
    }

    /// Find the existing Inbox phase (id `Inbox.0`) or create one at the end of
    /// the plan. Returns the assigned id for a freshly appended child.
    pub fn append_to_inbox(&mut self, subject: &str) -> String {
        if self.find("Inbox.0").is_none() {
            self.phases.push(Node {
                id: "Inbox.0".to_string(),
                title: "Inbox".to_string(),
                state: NodeState::Pending,
                children: vec![],
                annotations: vec![],
            });
        }
        let inbox = self.find_mut("Inbox.0").expect("just inserted");
        let next = next_inbox_child_id(inbox);
        inbox.children.push(Node {
            id: next.clone(),
            title: subject.to_string(),
            state: NodeState::Pending,
            children: vec![],
            annotations: vec![],
        });
        next
    }
}

fn remove_descendant(node: &mut Node, id: &str) -> Option<Node> {
    if let Some(idx) = node.children.iter().position(|c| c.id == id) {
        return Some(node.children.remove(idx));
    }
    for child in &mut node.children {
        if let Some(detached) = remove_descendant(child, id) {
            return Some(detached);
        }
    }
    None
}

fn next_inbox_child_id(inbox: &Node) -> String {
    let used: std::collections::HashSet<u32> = inbox
        .children
        .iter()
        .filter_map(|c| c.id.strip_prefix("Inbox."))
        .filter_map(|tail| tail.parse::<u32>().ok())
        .collect();
    let mut n = 1u32;
    while used.contains(&n) {
        n += 1;
    }
    format!("Inbox.{n}")
}

/// Insert `new_node` into `siblings` at the first position whose id sorts
/// strictly after the new id (per `cmp_ids`). When the new id is the largest,
/// this is just an append.
fn insert_in_order(siblings: &mut Vec<Node>, new_node: Node) {
    let pos = siblings
        .iter()
        .position(|n| cmp_ids(&new_node.id, &n.id) == std::cmp::Ordering::Less)
        .unwrap_or(siblings.len());
    siblings.insert(pos, new_node);
}

/// Compare two plan-path ids component-wise. Each `.`-separated component is
/// split into (numeric prefix, alpha suffix); numeric prefixes compare
/// numerically (so `1.10` > `1.9`), then suffixes compare lex with empty < any
/// non-empty suffix (so `1.2` < `1.2a` < `1.2b` < `1.3`). Components with no
/// numeric prefix fall back to full lex compare. Shorter ids sort before
/// longer ones sharing the same prefix (so `7.2` < `7.2.1`).
pub fn cmp_ids(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let a_parts: Vec<&str> = a.split('.').collect();
    let b_parts: Vec<&str> = b.split('.').collect();
    for (ap, bp) in a_parts.iter().zip(b_parts.iter()) {
        match cmp_component(ap, bp) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    a_parts.len().cmp(&b_parts.len())
}

fn cmp_component(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (an, asuf) = split_numeric_prefix(a);
    let (bn, bsuf) = split_numeric_prefix(b);
    match (an, bn) {
        (Some(a), Some(b)) => a.cmp(&b).then_with(|| asuf.cmp(bsuf)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

fn split_numeric_prefix(s: &str) -> (Option<u64>, &str) {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num_str, rest) = s.split_at(end);
    let num = if num_str.is_empty() {
        None
    } else {
        num_str.parse::<u64>().ok()
    };
    (num, rest)
}

/// Derive the parent id for a canonical plan_path. Returns None for top-level
/// (e.g. `1.0`, `Inbox.0`). For 2-part non-`.0` ids like `1.1` the parent is
/// `1.0` (the phase). For 3+ parts, parent is the prefix without the last
/// segment.
pub fn parent_id_for(plan_path: &str) -> Option<String> {
    let parts: Vec<&str> = plan_path.split('.').collect();
    match parts.as_slice() {
        [] | [_] => None,
        [_, "0"] => None,
        [head, _] => Some(format!("{head}.0")),
        many => Some(many[..many.len() - 1].join(".")),
    }
}

/// A non-checkbox line attached to a node — context for the work, not work itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Annotation {
    /// Plain prose.
    Text { text: String, indent: usize },
    /// A `- something` bullet without a checkbox.
    Bullet { text: String, indent: usize },
    /// A fenced code block.
    CodeBlock {
        lang: Option<String>,
        content: String,
        indent: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_preserves_plan() {
        let plan = Plan {
            preamble: vec!["# Header".to_string(), "".to_string()],
            phases: vec![Node {
                id: "1.0".to_string(),
                title: "Phase".to_string(),
                state: NodeState::Pending,
                annotations: vec![Annotation::Text {
                    text: "note".to_string(),
                    indent: 2,
                }],
                children: vec![Node {
                    id: "1.1".to_string(),
                    title: "Task".to_string(),
                    state: NodeState::Done,
                    children: vec![],
                    annotations: vec![],
                }],
            }],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let back: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, back);
    }

    #[test]
    fn parent_id_for_handles_canonical_shapes() {
        assert_eq!(parent_id_for("1.0"), None);
        assert_eq!(parent_id_for("Inbox.0"), None);
        assert_eq!(parent_id_for("1"), None);
        assert_eq!(parent_id_for("1.1"), Some("1.0".to_string()));
        assert_eq!(parent_id_for("Inbox.3"), Some("Inbox.0".to_string()));
        assert_eq!(parent_id_for("1.1.1"), Some("1.1".to_string()));
        assert_eq!(parent_id_for("X.4.a.1"), Some("X.4.a".to_string()));
    }

    #[test]
    fn find_walks_the_tree() {
        let plan = Plan {
            preamble: vec![],
            phases: vec![Node {
                id: "1.0".to_string(),
                title: "Phase".to_string(),
                state: NodeState::Pending,
                children: vec![Node {
                    id: "1.1".to_string(),
                    title: "Task".to_string(),
                    state: NodeState::Pending,
                    children: vec![Node {
                        id: "1.1.1".to_string(),
                        title: "Sub".to_string(),
                        state: NodeState::Done,
                        children: vec![],
                        annotations: vec![],
                    }],
                    annotations: vec![],
                }],
                annotations: vec![],
            }],
        };
        assert!(plan.find("1.0").is_some());
        assert!(plan.find("1.1").is_some());
        assert!(plan.find("1.1.1").is_some());
        assert!(plan.find("1.2").is_none());
        assert!(plan.find("Inbox.0").is_none());
    }

    #[test]
    fn add_child_of_appends() {
        let mut plan = Plan {
            preamble: vec![],
            phases: vec![Node {
                id: "1.0".to_string(),
                title: "Phase".to_string(),
                state: NodeState::Pending,
                children: vec![],
                annotations: vec![],
            }],
        };
        let child = Node {
            id: "1.1".to_string(),
            title: "Task".to_string(),
            state: NodeState::Pending,
            children: vec![],
            annotations: vec![],
        };
        plan.add_child_of("1.0", child).unwrap();
        assert_eq!(plan.find("1.1").unwrap().title, "Task");
    }

    #[test]
    fn cmp_ids_numeric_components_use_numeric_compare() {
        use std::cmp::Ordering;
        assert_eq!(cmp_ids("1.1", "1.2"), Ordering::Less);
        // Numeric, not lex: 1.10 > 1.9 (lex would say "1.10" < "1.9").
        assert_eq!(cmp_ids("1.10", "1.9"), Ordering::Greater);
        assert_eq!(cmp_ids("1.9", "1.10"), Ordering::Less);
        assert_eq!(cmp_ids("1.1", "1.1"), Ordering::Equal);
    }

    #[test]
    fn cmp_ids_alpha_suffix_orders_between_numerics() {
        use std::cmp::Ordering;
        // Empty suffix sorts before any non-empty suffix.
        assert_eq!(cmp_ids("7.2", "7.2a"), Ordering::Less);
        assert_eq!(cmp_ids("7.2a", "7.2b"), Ordering::Less);
        // Suffixed component still less than the next integer component.
        assert_eq!(cmp_ids("7.2a", "7.3"), Ordering::Less);
        assert_eq!(cmp_ids("7.2", "7.3"), Ordering::Less);
    }

    #[test]
    fn cmp_ids_shorter_id_sorts_first_under_same_prefix() {
        use std::cmp::Ordering;
        assert_eq!(cmp_ids("7.2", "7.2.1"), Ordering::Less);
        assert_eq!(cmp_ids("7.2.1", "7.2"), Ordering::Greater);
    }

    #[test]
    fn add_child_of_inserts_in_id_order_not_just_append() {
        // Regression for 7.7: given children [7.1, 7.2, 7.3], inserting `7.2a`
        // must land between 7.2 and 7.3, not at the end.
        let mut plan =
            parse_for_test("- [ ] 7.0 Phase\n  - [ ] 7.1 a\n  - [ ] 7.2 b\n  - [ ] 7.3 c\n");
        let new_child = Node {
            id: "7.2a".to_string(),
            title: "between".to_string(),
            state: NodeState::Pending,
            children: vec![],
            annotations: vec![],
        };
        plan.add_child_of("7.0", new_child).unwrap();
        let parent = plan.find("7.0").unwrap();
        let ids: Vec<&str> = parent.children.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["7.1", "7.2", "7.2a", "7.3"]);
    }

    #[test]
    fn add_child_of_prepends_when_new_id_is_smallest() {
        let mut plan = parse_for_test("- [ ] 1.0 Phase\n  - [ ] 1.5 mid\n  - [ ] 1.9 last\n");
        let new_child = Node {
            id: "1.1".to_string(),
            title: "first".to_string(),
            state: NodeState::Pending,
            children: vec![],
            annotations: vec![],
        };
        plan.add_child_of("1.0", new_child).unwrap();
        let ids: Vec<&str> = plan
            .find("1.0")
            .unwrap()
            .children
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(ids, vec!["1.1", "1.5", "1.9"]);
    }

    #[test]
    fn insert_phase_orders_top_level_too() {
        // Symmetry: top-level phases use the same ordering as child insertion.
        let mut plan = parse_for_test("- [ ] 1.0 a\n- [ ] 3.0 c\n");
        plan.insert_phase(Node {
            id: "2.0".to_string(),
            title: "b".to_string(),
            state: NodeState::Pending,
            children: vec![],
            annotations: vec![],
        });
        let ids: Vec<&str> = plan.phases.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["1.0", "2.0", "3.0"]);
    }

    #[test]
    fn add_child_of_errors_when_parent_missing() {
        let mut plan = Plan::default();
        let child = Node {
            id: "1.1".to_string(),
            title: "Task".to_string(),
            state: NodeState::Pending,
            children: vec![],
            annotations: vec![],
        };
        let err = plan.add_child_of("nope", child).unwrap_err();
        assert!(err.contains("nope"));
    }

    #[test]
    fn append_to_inbox_creates_phase_when_missing() {
        let mut plan = Plan::default();
        let assigned = plan.append_to_inbox("first inbox item");
        assert_eq!(assigned, "Inbox.1");
        assert!(plan.find("Inbox.0").is_some());
        assert!(plan.find("Inbox.1").is_some());
    }

    #[test]
    fn append_to_inbox_increments() {
        let mut plan = Plan::default();
        let a = plan.append_to_inbox("first");
        let b = plan.append_to_inbox("second");
        let c = plan.append_to_inbox("third");
        assert_eq!(a, "Inbox.1");
        assert_eq!(b, "Inbox.2");
        assert_eq!(c, "Inbox.3");
    }

    #[test]
    fn remove_pulls_a_leaf() {
        let mut plan = parse_for_test("- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        let removed = plan.remove("1.1").unwrap();
        assert_eq!(removed.id, "1.1");
        assert!(plan.find("1.1").is_none());
        assert!(plan.find("1.0").is_some(), "parent should remain");
    }

    #[test]
    fn remove_pulls_a_top_level_phase() {
        let mut plan = parse_for_test("- [ ] 1.0 P1\n- [ ] 2.0 P2\n");
        plan.remove("1.0").unwrap();
        assert!(plan.find("1.0").is_none());
        assert!(plan.find("2.0").is_some());
    }

    #[test]
    fn remove_returns_none_when_missing() {
        let mut plan = parse_for_test("- [ ] 1.0 P\n");
        assert!(plan.remove("nope").is_none());
    }

    fn parse_for_test(input: &str) -> Plan {
        crate::parser::parse(input).unwrap()
    }

    #[test]
    fn append_to_inbox_skips_used_ids() {
        let mut plan = Plan {
            preamble: vec![],
            phases: vec![Node {
                id: "Inbox.0".to_string(),
                title: "Inbox".to_string(),
                state: NodeState::Pending,
                children: vec![
                    Node {
                        id: "Inbox.1".to_string(),
                        title: "x".to_string(),
                        state: NodeState::Pending,
                        children: vec![],
                        annotations: vec![],
                    },
                    Node {
                        id: "Inbox.3".to_string(),
                        title: "y".to_string(),
                        state: NodeState::Pending,
                        children: vec![],
                        annotations: vec![],
                    },
                ],
                annotations: vec![],
            }],
        };
        let next = plan.append_to_inbox("fills the gap");
        assert_eq!(next, "Inbox.2");
    }

    #[test]
    fn standardize_passes_through_canonical_plan() {
        let plan = parse_for_test("- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n- [ ] 2.0 Another\n");
        let (out, notes) = plan.clone().standardize_to_canonical().unwrap();
        assert!(notes.is_empty(), "no conversions on canonical input");
        assert_eq!(out.phases.len(), plan.phases.len());
    }

    #[test]
    fn standardize_passes_through_headers_in_preamble() {
        // Preamble headers are preserved verbatim; not in-tree → no rewrite.
        let plan = parse_for_test(
            "# Project\n\n## Goal\n\nSome prose.\n\n- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n",
        );
        let (_, notes) = plan.standardize_to_canonical().unwrap();
        assert!(
            notes.is_empty(),
            "preamble headers shouldn't trigger conversion"
        );
    }

    #[test]
    fn standardize_promotes_phase_n_header_to_canonical_phase() {
        // The shakeout shape: `### Phase 1 — Build` between checkboxes.
        // After standardize: phases 1.1+ become children of a new 1.0 node.
        let plan = parse_for_test(
            "- [ ] 0.1 First\n- [ ] 0.5 Last in zero\n\n### Phase 1 — Build\n\n- [ ] 1.1 Build it\n- [ ] 1.2 Build more\n",
        );
        let (out, notes) = plan.standardize_to_canonical().unwrap();
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].contains("1.0"),
            "note should call out promotion: {notes:?}"
        );

        let ids: Vec<&str> = out.phases.iter().map(|n| n.id.as_str()).collect();
        // 0.1 and 0.5 are orphans (no preceding Phase header for them), stay top-level.
        // 1.0 is the synthesized phase parent, with 1.1 and 1.2 as children.
        assert_eq!(ids, vec!["0.1", "0.5", "1.0"], "got phases: {ids:?}");
        let p10 = out.phases.iter().find(|n| n.id == "1.0").unwrap();
        assert_eq!(p10.title, "Build");
        let child_ids: Vec<&str> = p10.children.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(child_ids, vec!["1.1", "1.2"]);
    }

    #[test]
    fn standardize_handles_dotted_phase_numbers() {
        // `### Phase 3.5 — Titles` → id="3.5", title="Titles". Verbatim dot preserved.
        let plan = parse_for_test(
            "- [ ] 0.1 First\n\n### Phase 3.5 — Titles via vision OCR [done]\n\n- [ ] 3.5.1 Sub\n",
        );
        let (out, notes) = plan.standardize_to_canonical().unwrap();
        assert_eq!(notes.len(), 1);
        let p35 = out
            .phases
            .iter()
            .find(|n| n.id == "3.5")
            .expect("3.5 created");
        assert_eq!(p35.title, "Titles via vision OCR [done]");
    }

    #[test]
    fn standardize_promotes_alphanumeric_id_heading() {
        // Phase 16.3 — quicksight shakeout. `### AA.A — Title` (no "Phase"
        // keyword, alphanumeric id) should also promote to canonical.
        let plan = parse_for_test(
            "- [ ] 0.1 First\n\n### AA.A — Dropdown-control flip\n\n- [ ] AA.A.1 Audit\n",
        );
        let (out, notes) = plan.standardize_to_canonical().unwrap();
        assert_eq!(notes.len(), 1);
        let p_aa_a = out
            .phases
            .iter()
            .find(|n| n.id == "AA.A")
            .expect("AA.A phase created");
        assert_eq!(p_aa_a.title, "Dropdown-control flip");
        let child_ids: Vec<&str> = p_aa_a.children.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(child_ids, vec!["AA.A.1"]);
    }

    #[test]
    fn standardize_alpha_only_id_gets_zero_appended() {
        // `### AA — Title` → id="AA.0" (children like "AA.1" → parent "AA.0").
        let plan =
            parse_for_test("- [ ] 0.1 First\n\n### AA — Top-level alpha\n\n- [ ] AA.1 Sub\n");
        let (out, _) = plan.standardize_to_canonical().unwrap();
        let p_aa = out
            .phases
            .iter()
            .find(|n| n.id == "AA.0")
            .expect("AA.0 phase created");
        assert_eq!(p_aa.title, "Top-level alpha");
    }

    #[test]
    fn standardize_leaves_generic_heading_without_separator_alone() {
        // Phase 19 — `### Architecture` doesn't match Phase-N shape (no
        // separator) → stays as an annotation, no refusal. Original column
        // 0 indent is preserved via serializer.
        let plan = parse_for_test("- [ ] 0.1 First\n\n### Architecture\n\n- [ ] 1.0 Phase\n");
        let (out, _) = plan
            .standardize_to_canonical()
            .expect("non-matching headers no longer refused");
        // No promotion — original phases remain (0.1 and 1.0 at top level).
        let ids: Vec<&str> = out.phases.iter().map(|n| n.id.as_str()).collect();
        assert!(
            ids.contains(&"0.1") && ids.contains(&"1.0"),
            "phases preserved: {ids:?}"
        );
    }

    #[test]
    fn standardize_leaves_unrecognized_headers_alone() {
        // `## Notes` stays as narrative. Plan parses, standardizes, and the
        // annotation survives on whichever node the parser attached it to.
        let plan = parse_for_test("- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n\n## Notes\n\nSome stuff.\n");
        let (out, _) = plan.standardize_to_canonical().unwrap();
        // Look for `## Notes` text annotation anywhere in the resulting tree.
        let found = out.phases.iter().any(|p| {
            p.annotations
                .iter()
                .any(|a| matches!(a, Annotation::Text { text, .. } if text.contains("## Notes")))
                || p.children.iter().any(|c| {
                    c.annotations.iter().any(
                        |a| matches!(a, Annotation::Text { text, .. } if text.contains("## Notes")),
                    )
                })
        });
        assert!(found, "## Notes should remain as annotation");
    }

    #[test]
    fn standardize_leaves_phase_with_slash_alone() {
        // `Phase 2/3` isn't a valid id token (the `/`) → no promotion, but
        // also no refusal. Stays as narrative.
        let plan =
            parse_for_test("- [ ] 0.1 First\n\n### Phase 2/3 — Batch pipeline\n\n- [ ] 2.1 Sub\n");
        let (out, _) = plan
            .standardize_to_canonical()
            .expect("Phase 2/3 stays as narrative");
        // No `2.0` or `2/3.0` phase synthesized; 0.1 and 2.1 stay top-level.
        let ids: Vec<&str> = out.phases.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"0.1") && ids.contains(&"2.1"), "got: {ids:?}");
    }

    #[test]
    fn standardize_skips_promotion_for_deep_hash_headers() {
        // Phase 19 — `####+` headers are sub-section labels inside a phase;
        // they don't form phase boundaries. Even though `#### X.4.a — ...`
        // matches the `<id> — Title` shape, depth 4 disqualifies it.
        let plan = parse_for_test(
            "- [ ] X.0 Phase\n  - [ ] X.1 Sub\n\n#### X.4.a — Foundations\n\n- [ ] X.4.a.1 Detail\n",
        );
        let (out, _) = plan.standardize_to_canonical().unwrap();
        // No `X.4.a` phase synthesized — X.4.a.1 stays top-level.
        let ids: Vec<&str> = out.phases.iter().map(|n| n.id.as_str()).collect();
        assert!(!ids.contains(&"X.4.a"), "should NOT promote ####: {ids:?}");
        assert!(ids.contains(&"X.4.a.1"), "X.4.a.1 stays top-level: {ids:?}");
    }

    #[test]
    fn standardize_ignores_text_starting_with_hash_but_not_header() {
        // `#hashtag` (no space after #) isn't a markdown header → not collected.
        let plan = Plan {
            preamble: vec![],
            phases: vec![Node {
                id: "1.0".to_string(),
                title: "Phase".to_string(),
                state: NodeState::Pending,
                children: vec![],
                annotations: vec![Annotation::Text {
                    text: "#hashtag style not a header".to_string(),
                    indent: 2,
                }],
            }],
        };
        let (_, notes) = plan.standardize_to_canonical().unwrap();
        assert!(notes.is_empty());
    }

    #[test]
    fn json_uses_kind_tag_for_annotations() {
        let ann = Annotation::Bullet {
            text: "x".to_string(),
            indent: 2,
        };
        let json = serde_json::to_string(&ann).unwrap();
        assert!(json.contains("\"kind\":\"bullet\""), "got: {json}");
    }
}

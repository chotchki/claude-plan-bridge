use serde::{Deserialize, Serialize};

/// A parsed PLAN.md document.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// Lines preceding the first checkbox node, preserved verbatim for round-trip.
    pub preamble: Vec<String>,
    /// Top-level phases. Phase 36 split: phases are no longer Node-shaped
    /// checkboxes (`- [ ] N.0 ...`) but their own struct, so FORMATv2's header
    /// form (`## Phase N - Title *(depends on: ...)*`) has a real home. v1
    /// PLAN.md files still parse — the parser converts each top-level
    /// `- [ ] N.0` line into a `Phase` with the `state`/`id_style`/`separator`
    /// fields preserving the legacy anchor form so the serializer can round-trip
    /// it (Phase 37 will switch the serializer to FORMATv2 by default).
    pub phases: Vec<Phase>,
    /// The canonical `## Backlog (not yet phased)` section, owned as a
    /// first-class trailing region so it always serializes at the very bottom
    /// (below every phase) and survives phase-appends without drifting. Holds
    /// the verbatim body lines (bullets) under the heading — the heading itself
    /// is re-emitted by the serializer. Empty when the plan has no backlog, or
    /// when the backlog is still living in the preamble / as annotations
    /// (un-consolidated). The parser auto-lifts a *trailing* backlog block here;
    /// `consolidate_backlog` sweeps the rest. See [Plan::append_backlog_note].
    #[serde(default)]
    pub backlog: Vec<String>,
    /// When `true`, the serializer emits the backlog heading as `# Backlog
    /// (not yet phased)` (FORMATv2 canonical, h1). When `false`, the legacy
    /// `## Backlog (not yet phased)` (h2). Set by the parser when it sees
    /// `# Backlog` on read, and by `canonicalize` when flipping the plan to
    /// v2. Routine writes preserve the parsed value.
    #[serde(default)]
    pub backlog_h1: bool,
}

/// A top-level phase. Tasks live in `children`; phase-level metadata
/// (`depends_on`, eventually FORMATv2 prose) live on the Phase itself.
///
/// FORMATv2-only: a phase is always a `## Phase X - Title` header. Phases have
/// no checkbox of their own — completion is derived from their leaves — so the
/// `state` field is advisory only (kept for state-file back-compat and the
/// rare childless-phase render); routine flows derive completion from leaves.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub state: NodeState,
    /// Tasks under the phase. Field is named `children` (rather than `tasks`)
    /// for 36.1 so consumers that read `phase.children` keep compiling without
    /// a rename sweep; 36.2 will tighten the naming.
    #[serde(default)]
    pub children: Vec<Node>,
    /// Annotations attached at the phase level — prose under a `## Phase`
    /// header not attached to any task.
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    /// FORMATv2: phases declared with `*(depends on: AB, AC)*` carry the
    /// dependency list here. Informational only — reconcile surfaces it; the
    /// bridge does not enforce ordering at archive time. Reconcile language is
    /// strong ("AS depends on AR — AR not yet archived").
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// FORMATv2: phases declared with `*(prefer after: AB)*` carry a softer
    /// sequencing hint here — work that benefits from being done after the
    /// listed phases but isn't blocked by them. Same informational-only
    /// posture as [Phase::depends_on], but reconcile speaks more gently
    /// ("AS prefers AR has landed first").
    #[serde(default)]
    pub prefer_after: Vec<String>,
}

/// A single checkbox node in the plan. Tasks and subtasks share this shape;
/// depth is determined by the dotted `id` (e.g., `1.1`, `1.1.1`) and by tree
/// position. Top-level phases use [Phase] instead.
///
/// FORMATv2-only: the id is always plain (no bold) and the on-disk separator is
/// always ` - ` (hyphen-space). Reads tolerate a bare space between id and
/// title; the serializer normalizes every line back to ` - `.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub state: NodeState,
    pub children: Vec<Node>,
    pub annotations: Vec<Annotation>,
}

/// Checkbox state. `Pending` = `[ ]`, `Done` = `[x]`, `WontDo` = `[-]`, `Backlog` = `[>]`.
///
/// `Done`, `WontDo`, and `Backlog` are all "resolved" — archive treats them
/// equivalently — but they're semantically distinct in PLAN.md:
/// - `WontDo` = *we decided not to do this*
/// - `Backlog` = *deferred from this phase; worth keeping for later*
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    #[default]
    Pending,
    Done,
    WontDo,
    Backlog,
}

impl NodeState {
    /// True when this leaf is no longer active work — done, skipped, or
    /// deferred. Archive uses this; reconcile draws a finer line.
    pub fn is_resolved(self) -> bool {
        matches!(
            self,
            NodeState::Done | NodeState::WontDo | NodeState::Backlog
        )
    }

    /// Single-glyph rendering for human-facing surfaces — reconcile drift,
    /// hook additionalContext, status output. The serializer (PLAN.md on disk)
    /// always uses bracket form; this is presentation-only.
    pub fn emoji(self) -> &'static str {
        match self {
            NodeState::Pending => "⬜",
            NodeState::Done => "✅",
            NodeState::WontDo => "❌",
            NodeState::Backlog => "🔜",
        }
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

impl Phase {
    /// True when the phase carries no tasks. Phases are not "leaves" in the
    /// task sense — this is for traversal symmetry with [Node::is_leaf].
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    pub fn is_done(&self) -> bool {
        self.state == NodeState::Done
    }

    pub fn is_resolved(&self) -> bool {
        self.state.is_resolved()
    }

    /// Collect every leaf under this phase (depth-first, document order).
    /// Phases themselves are not leaves; only childless tasks/subtasks are.
    pub fn leaves(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        for child in &self.children {
            collect_leaves(child, &mut out);
        }
        out
    }

    /// Search this phase + its task subtree for an id. Matches the phase
    /// itself when `id` equals `self.id`; otherwise recurses into tasks.
    /// Returned reference is `&Node` — phase matches are returned as a
    /// borrowed [Node] view via [Phase::as_node_view]; if you need the
    /// `Phase` itself, look it up on [Plan::phases] directly.
    pub fn find_task(&self, id: &str) -> Option<&Node> {
        for child in &self.children {
            if let Some(n) = child.find(id) {
                return Some(n);
            }
        }
        None
    }

    pub fn find_task_mut(&mut self, id: &str) -> Option<&mut Node> {
        for child in &mut self.children {
            if let Some(n) = child.find_mut(id) {
                return Some(n);
            }
        }
        None
    }

    /// Build a Phase from a Node — wraps a node's id/title/children/annotations
    /// into the phase tier for insertion (e.g. a TaskCreate whose `plan_path`
    /// is a bare phase id, or a phase detached and re-inserted by archive).
    pub fn from_node(node: Node) -> Self {
        Self {
            id: node.id,
            title: node.title,
            state: node.state,
            children: node.children,
            annotations: node.annotations,
            depends_on: Vec::new(),
            prefer_after: Vec::new(),
        }
    }

    /// Build a fresh FORMATv2 phase header (state=Pending; tasks/annotations/
    /// deps all empty). The minimal constructor — for the "I want a new phase,
    /// deps come later" case.
    pub fn header_v2(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            state: NodeState::Pending,
            children: Vec::new(),
            annotations: Vec::new(),
            depends_on: Vec::new(),
            prefer_after: Vec::new(),
        }
    }

    /// Build a fresh FORMATv2 phase header pre-populated with dep markers — for
    /// the `plan_add_phase(id, title, depends_on, prefer_after)` flow that wants
    /// every field at construction time.
    pub fn header_v2_with_deps(
        id: impl Into<String>,
        title: impl Into<String>,
        depends_on: Vec<String>,
        prefer_after: Vec<String>,
    ) -> Self {
        Self {
            depends_on,
            prefer_after,
            ..Self::header_v2(id, title)
        }
    }
}

/// Borrowed view of either a Phase or a Node — returned by
/// [Plan::find_item] for callers that need to inspect common fields without
/// caring which tier the id lives at.
#[derive(Debug)]
pub enum PlanItemRef<'a> {
    Phase(&'a Phase),
    Node(&'a Node),
}

impl<'a> PlanItemRef<'a> {
    pub fn id(&self) -> &str {
        match self {
            Self::Phase(p) => &p.id,
            Self::Node(n) => &n.id,
        }
    }
    pub fn title(&self) -> &str {
        match self {
            Self::Phase(p) => &p.title,
            Self::Node(n) => &n.title,
        }
    }
    pub fn state(&self) -> NodeState {
        match self {
            Self::Phase(p) => p.state,
            Self::Node(n) => n.state,
        }
    }
    pub fn annotations(&self) -> &[Annotation] {
        match self {
            Self::Phase(p) => &p.annotations,
            Self::Node(n) => &n.annotations,
        }
    }
    pub fn children(&self) -> &[Node] {
        match self {
            Self::Phase(p) => &p.children,
            Self::Node(n) => &n.children,
        }
    }
    pub fn is_leaf(&self) -> bool {
        self.children().is_empty()
    }
}

/// Mutable view of either a Phase or a Node — returned by
/// [Plan::find_item_mut]. Methods provide tier-uniform mutation of the
/// fields shared between Phase and Node.
#[derive(Debug)]
pub enum PlanItemMut<'a> {
    Phase(&'a mut Phase),
    Node(&'a mut Node),
}

impl<'a> PlanItemMut<'a> {
    pub fn id(&self) -> &str {
        match self {
            Self::Phase(p) => &p.id,
            Self::Node(n) => &n.id,
        }
    }
    pub fn title(&self) -> &str {
        match self {
            Self::Phase(p) => &p.title,
            Self::Node(n) => &n.title,
        }
    }
    pub fn set_title(&mut self, t: String) {
        match self {
            Self::Phase(p) => p.title = t,
            Self::Node(n) => n.title = t,
        }
    }
    pub fn state(&self) -> NodeState {
        match self {
            Self::Phase(p) => p.state,
            Self::Node(n) => n.state,
        }
    }
    pub fn set_state(&mut self, s: NodeState) {
        match self {
            Self::Phase(p) => p.state = s,
            Self::Node(n) => n.state = s,
        }
    }
    pub fn annotations(&self) -> &[Annotation] {
        match self {
            Self::Phase(p) => &p.annotations,
            Self::Node(n) => &n.annotations,
        }
    }
    pub fn annotations_mut(&mut self) -> &mut Vec<Annotation> {
        match self {
            Self::Phase(p) => &mut p.annotations,
            Self::Node(n) => &mut n.annotations,
        }
    }
    pub fn children(&self) -> &[Node] {
        match self {
            Self::Phase(p) => &p.children,
            Self::Node(n) => &n.children,
        }
    }
    pub fn children_mut(&mut self) -> &mut Vec<Node> {
        match self {
            Self::Phase(p) => &mut p.children,
            Self::Node(n) => &mut n.children,
        }
    }
    pub fn is_leaf(&self) -> bool {
        self.children().is_empty()
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

/// True for the canonical bridge-owned Backlog heading. Accepts both
/// `# Backlog (not yet phased)` (FORMATv2 canonical, h1) and the legacy
/// `## Backlog (not yet phased)` (h2 — written by pre-37 versions of the
/// bridge). Phase 37.2 will flip the serializer to emit h1.
///
/// Deliberately conservative: matches the bridge's own heading but NOT the
/// h3 `### Backlog (rehomed from AA)` subsection nor a `## Sustainment`-style
/// sibling — those are operator-curated and must not be swept into the
/// bridge's bottom section. Exact prefix match (`# ` or `## ` followed by
/// `Backlog`) is what keeps `### Backlog` and `#### …` out.
pub fn is_backlog_heading(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("# Backlog") || trimmed.starts_with("## Backlog")
}

/// Phase CG: one grouped backlog item — a top-level bullet plus everything
/// beneath it up to the next top-level bullet. `start`/`len` index into
/// `Plan::backlog` so a promote can drain the exact source lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogEntry {
    /// The promotion-ready title: the top bullet's text, cleaned of `**`/`~~`
    /// and (if present) split off from a ` — ` detail tail or a `**bold**` lead.
    pub headline: String,
    /// The remaining stanza: any text after the headline on the top line, plus
    /// every sub-bullet / prose / blank line beneath it (verbatim).
    pub detail: Vec<String>,
    /// Index of the top bullet line in `Plan::backlog`.
    pub start: usize,
    /// Number of raw lines this entry spans.
    pub len: usize,
}

/// A backlog entry boundary: a `- ` bullet at column 0 (no leading whitespace).
fn is_top_level_bullet(line: &str) -> bool {
    line.starts_with("- ")
}

/// Split a top-level backlog bullet into (headline, first-line detail). Handles
/// the two shapes the bridge writes (`- **Title** — meta` and `- Title — meta`)
/// plus a plain `- Title`. `**`/`~~` wrappers are stripped from the headline.
fn split_headline(top_line: &str) -> (String, String) {
    let body = top_line.strip_prefix("- ").unwrap_or(top_line).trim();
    // `**Headline** rest` — the bold span is the headline, the remainder detail.
    if let Some(after_open) = body.strip_prefix("**")
        && let Some(close) = after_open.find("**")
    {
        let headline = clean_headline(&after_open[..close]);
        let rest = after_open[close + 2..]
            .trim()
            .trim_start_matches('—')
            .trim_start_matches('-')
            .trim()
            .to_string();
        return (headline, rest);
    }
    // `Headline — detail` (em-dash separator the bridge uses for notes).
    if let Some((h, rest)) = body.split_once(" — ") {
        return (clean_headline(h), rest.trim().to_string());
    }
    (clean_headline(body), String::new())
}

/// Strip `**`/`~~` markdown decoration (anywhere in the string) and surrounding
/// whitespace from a headline so it reads as a clean phase title.
fn clean_headline(s: &str) -> String {
    s.replace("**", "").replace("~~", "").trim().to_string()
}

/// Convert a raw backlog detail line into a phase-level annotation, preserving
/// indent. Bullets stay bullets, blanks stay blanks, everything else is text.
fn line_to_annotation(line: &str) -> Annotation {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        Annotation::Blank { count: 1 }
    } else if let Some(rest) = trimmed.strip_prefix("- ") {
        Annotation::Bullet {
            text: rest.to_string(),
            indent,
        }
    } else {
        Annotation::Text {
            text: trimmed.to_string(),
            indent,
        }
    }
}

impl Plan {
    /// Every leaf across all phases, returned as a uniform Phase-or-Node view.
    /// A *phase* qualifies as a leaf only in the degenerate case where it has
    /// no tasks under it (a `## Phase X` header not yet broken down). A *task*
    /// qualifies as a leaf when it has no nested children. Document order:
    /// childless phase emits its own item; non-empty phase emits each leaf
    /// descendant of its task subtree.
    pub fn leaves(&self) -> Vec<PlanItemRef<'_>> {
        let mut out: Vec<PlanItemRef<'_>> = Vec::new();
        for phase in &self.phases {
            if phase.children.is_empty() {
                out.push(PlanItemRef::Phase(phase));
                continue;
            }
            let mut node_leaves: Vec<&Node> = Vec::new();
            for child in &phase.children {
                collect_leaves(child, &mut node_leaves);
            }
            for leaf in node_leaves {
                out.push(PlanItemRef::Node(leaf));
            }
        }
        out
    }

    /// Full-tree search by id, walking *task* subtrees only. For top-level
    /// phase-id lookups use [Plan::find_phase]; for the common "is this id
    /// anywhere in the plan?" check use [Plan::contains_id]; for callers that
    /// don't know whether `id` names a phase or a task use [Plan::find_item].
    pub fn find(&self, id: &str) -> Option<&Node> {
        for p in &self.phases {
            if let Some(n) = p.find_task(id) {
                return Some(n);
            }
        }
        None
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut Node> {
        for p in &mut self.phases {
            if let Some(n) = p.find_task_mut(id) {
                return Some(n);
            }
        }
        None
    }

    /// Find a phase by id at the top level.
    pub fn find_phase(&self, id: &str) -> Option<&Phase> {
        self.phases.iter().find(|p| phase_id_matches(&p.id, id))
    }

    pub fn find_phase_mut(&mut self, id: &str) -> Option<&mut Phase> {
        self.phases.iter_mut().find(|p| phase_id_matches(&p.id, id))
    }

    /// True when `id` matches any phase OR any task in the plan. Cheap
    /// existence check used by writeback / mcp where the caller doesn't care
    /// what kind of node it is, only that something owns the id.
    pub fn contains_id(&self, id: &str) -> bool {
        self.find_phase(id).is_some() || self.find(id).is_some()
    }

    /// Locate a phase OR task by id, returning whichever variant matched.
    /// Use when the caller needs to read shared fields (id/title/state/
    /// annotations) regardless of which tier the id lives at.
    pub fn find_item(&self, id: &str) -> Option<PlanItemRef<'_>> {
        if let Some(p) = self.find_phase(id) {
            return Some(PlanItemRef::Phase(p));
        }
        self.find(id).map(PlanItemRef::Node)
    }

    pub fn find_item_mut(&mut self, id: &str) -> Option<PlanItemMut<'_>> {
        if let Some(idx) = self.phases.iter().position(|p| phase_id_matches(&p.id, id)) {
            return Some(PlanItemMut::Phase(&mut self.phases[idx]));
        }
        for phase in &mut self.phases {
            if let Some(n) = phase.find_task_mut(id) {
                return Some(PlanItemMut::Node(n));
            }
        }
        None
    }

    /// Insert a child into the node with `parent_id`, positioned in id-sort
    /// order against its siblings. Lets `1.2a` land between `1.2` and `1.3`
    /// without renumbering. Returns Err if no such parent.
    ///
    /// Parent resolution: if `parent_id` matches a top-level Phase, the child
    /// lands in that phase's task list. Otherwise the bridge searches every
    /// phase's task subtree for a Node with the matching id.
    pub fn add_child_of(&mut self, parent_id: &str, child: Node) -> Result<(), String> {
        if let Some(phase) = self
            .phases
            .iter_mut()
            .find(|p| phase_id_matches(&p.id, parent_id))
        {
            insert_in_order(&mut phase.children, child);
            return Ok(());
        }
        for phase in &mut self.phases {
            if let Some(parent) = phase.find_task_mut(parent_id) {
                insert_in_order(&mut parent.children, child);
                return Ok(());
            }
        }
        Err(format!("no node with id {parent_id} in PLAN.md"))
    }

    /// Phase CE: append auto-numbered child tasks under an existing phase or
    /// task. `parent` may be a phase id (`CE`) or a task id at any depth
    /// (`CE.3`, `CE.3.2`); new children continue after the highest existing
    /// numeric suffix, so repeated calls keep appending. Returns the new child
    /// ids. Errs if the parent isn't found, or no non-empty subject is given.
    pub fn breakdown(&mut self, parent: &str, subjects: &[String]) -> Result<Vec<String>, String> {
        let existing: Vec<String> = if let Some(ph) = self.find_phase(parent) {
            ph.children.iter().map(|c| c.id.clone()).collect()
        } else if let Some(n) = self.find(parent) {
            n.children.iter().map(|c| c.id.clone()).collect()
        } else {
            return Err(format!("no phase or task with id `{parent}` in PLAN.md"));
        };
        let mut next = existing
            .iter()
            .filter_map(|id| id.rsplit('.').next().and_then(|s| s.parse::<u64>().ok()))
            .max()
            .unwrap_or(0)
            + 1;
        let mut added = Vec::new();
        for subject in subjects {
            let subject = subject.trim();
            if subject.is_empty() {
                continue;
            }
            let id = format!("{parent}.{next}");
            let child = Node {
                id: id.clone(),
                title: subject.to_string(),
                state: NodeState::Pending,
                children: vec![],
                annotations: vec![],
            };
            self.add_child_of(parent, child)?;
            added.push(id);
            next += 1;
        }
        if added.is_empty() {
            return Err("no non-empty task subjects given".to_string());
        }
        Ok(added)
    }

    /// Insert a top-level phase in id-sort order against existing phases.
    pub fn insert_phase(&mut self, phase: Phase) {
        insert_phase_in_order(&mut self.phases, phase);
    }

    /// Remove a node by id from anywhere in the tree. Returns the detached
    /// node when found. Does not cascade-remove orphaned empty parents
    /// (deliberate v1 decision per PLAN.md 2.3.3).
    ///
    /// Phase 36 split: removing a top-level *phase* returns the phase's
    /// fields wrapped back into a Node — preserves the contract for callers
    /// that store the result for later re-insertion (archive sweep, descope).
    pub fn remove(&mut self, id: &str) -> Option<Node> {
        if let Some(idx) = self.phases.iter().position(|p| p.id == id) {
            return Some(phase_to_node(self.phases.remove(idx)));
        }
        for phase in &mut self.phases {
            if let Some(detached) = remove_descendant_in_phase(phase, id) {
                return Some(detached);
            }
        }
        None
    }

    /// Remove a top-level phase by id, returning the full Phase (with its
    /// FORMATv2 metadata intact). Use this instead of [Plan::remove] when
    /// you specifically want a Phase back — archive sweeps, future descope
    /// flows.
    pub fn remove_phase(&mut self, id: &str) -> Option<Phase> {
        let idx = self.phases.iter().position(|p| p.id == id)?;
        Some(self.phases.remove(idx))
    }

    /// Append a no-path note to the canonical Backlog field (the bottom
    /// section). Used when a `TaskCreate` arrives with no `plan_path` — the
    /// work is real but unphased, so it lands in Backlog until a planning move
    /// promotes it. Idempotent on the exact bullet text.
    ///
    /// Format: `- **<title>** — added <date>.`
    pub fn append_backlog_note(&mut self, title: &str, date: &str) {
        let entry = format!("- **{title}** — added {date}.");
        if !self.backlog.contains(&entry) {
            self.backlog.push(entry);
        }
    }

    /// Append a deferral to the canonical Backlog field. Used by the defer
    /// paths (`plan_backlog`, `plan_skip`, `TaskUpdate(deleted)` on a pending
    /// leaf) once their content has been consolidated to the bottom section.
    /// Idempotent on the source `plan_path`.
    ///
    /// Format: `- **<title>** — deferred from <plan_path> on <date>.`
    pub fn append_backlog_deferral(&mut self, plan_path: &str, title: &str, date: &str) {
        let needle = format!("deferred from {plan_path} on");
        if self.backlog.iter().any(|line| line.contains(&needle)) {
            return;
        }
        self.backlog.push(format!(
            "- **{title}** — deferred from {plan_path} on {date}."
        ));
    }

    /// Phase 38.6: append a deferral as a FORMATv2 nested-subtree bullet.
    /// Top line: `- <id> - <title> *(deferred from phase <phase> on <date>)*`,
    /// followed by indented children for any descendants. Idempotent on the
    /// source plan_path — if the top-line already exists in backlog, this is
    /// a no-op (matches `append_backlog_deferral`'s idempotency).
    ///
    /// For leaf nodes the output is a single line; for non-leaf nodes the
    /// full subtree gets nested under it. Subtask state markers are dropped
    /// (FORMATv2 backlog entries are notes, not tracked work).
    pub fn append_backlog_subtree(&mut self, node: &Node, source_phase: &str, date: &str) {
        // Idempotency probe: match on the deferral marker for this plan_path
        // at the top level (avoids double-add if backlog already had it).
        let top_marker = format!("(deferred from phase `{source_phase}` on");
        let id_marker = format!("- {} -", node.id);
        let already = self
            .backlog
            .iter()
            .any(|line| line.trim_start().starts_with(&id_marker) && line.contains(&top_marker));
        if already {
            return;
        }
        // Top-line: id, title, deferral marker.
        let top = if node.title.is_empty() {
            format!(
                "- {} *(deferred from phase `{source_phase}` on {date})*",
                node.id
            )
        } else {
            format!(
                "- {} - {} *(deferred from phase `{source_phase}` on {date})*",
                node.id, node.title
            )
        };
        self.backlog.push(top);
        for child in &node.children {
            push_subtree_lines(&mut self.backlog, child, 1);
        }
    }

    /// Remove the first Backlog bullet whose bolded title matches `title`.
    /// Returns true when a line was removed. Used to clear a no-path note when
    /// its harness task is completed or deleted, so resolved backlog items
    /// don't linger as tracked-but-invisible entries.
    pub fn remove_backlog_note(&mut self, title: &str) -> bool {
        let marker = format!("**{title}**");
        if let Some(idx) = self.backlog.iter().position(|line| line.contains(&marker)) {
            self.backlog.remove(idx);
            true
        } else {
            false
        }
    }

    /// Rename a Backlog bullet's bolded title in place (`**old**` → `**new**`),
    /// preserving the rest of the line. Returns true when a line matched. Keeps
    /// the bullet's stored title aligned with a `TaskUpdate(subject=...)` so
    /// later `remove_backlog_note` can still find it.
    pub fn rename_backlog_note(&mut self, old: &str, new: &str) -> bool {
        let marker = format!("**{old}**");
        if let Some(line) = self.backlog.iter_mut().find(|l| l.contains(&marker)) {
            *line = line.replace(&marker, &format!("**{new}**"));
            true
        } else {
            false
        }
    }

    /// Phase CG: group the raw `backlog` lines into entries. The unit is a
    /// **top-level bullet** (`- ` at column 0); an entry is that bullet plus
    /// every following line (sub-bullets, indented prose, blanks) up to — but
    /// not including — the next top-level bullet. Lines before the first
    /// top-level bullet (stray preamble prose) are skipped. Tolerant by design:
    /// no on-disk format is enforced, so a hand-edited backlog still groups.
    pub fn backlog_entries(&self) -> Vec<BacklogEntry> {
        let mut entries = Vec::new();
        let mut i = 0;
        while i < self.backlog.len() {
            if !is_top_level_bullet(&self.backlog[i]) {
                i += 1;
                continue;
            }
            let start = i;
            let (headline, first_detail) = split_headline(&self.backlog[i]);
            let mut detail: Vec<String> = Vec::new();
            if !first_detail.is_empty() {
                detail.push(first_detail);
            }
            let mut j = i + 1;
            while j < self.backlog.len() && !is_top_level_bullet(&self.backlog[j]) {
                detail.push(self.backlog[j].clone());
                j += 1;
            }
            entries.push(BacklogEntry {
                headline,
                detail,
                start,
                len: j - start,
            });
            i = j;
        }
        entries
    }

    /// Phase CG: promote the `index`-th (1-based) backlog entry into a new
    /// top-level phase. The entry's headline becomes the phase title (overridable
    /// via `title`); the rest of the stanza becomes phase-level prose (NOT
    /// tasks — break it down separately). The promoted lines are removed from
    /// `backlog` and the new phase is inserted in id-sort order. Returns the
    /// phase title used. Errors when `index` is out of range.
    pub fn promote_backlog_entry(
        &mut self,
        index: usize,
        title: Option<&str>,
        new_id: &str,
    ) -> Result<String, String> {
        let entries = self.backlog_entries();
        let n = entries.len();
        let entry = index
            .checked_sub(1)
            .and_then(|zero| entries.get(zero))
            .ok_or_else(|| {
                if n == 0 {
                    "no backlog entries to promote".to_string()
                } else {
                    format!("backlog entry {index} out of range (1..={n})")
                }
            })?;

        let phase_title = title
            .map(str::to_string)
            .unwrap_or_else(|| entry.headline.clone());
        let (start, len) = (entry.start, entry.len);
        let annotations: Vec<Annotation> =
            entry.detail.iter().map(|l| line_to_annotation(l)).collect();

        let mut phase = Phase::header_v2(new_id, phase_title.clone());
        phase.annotations = annotations;

        // Remove the promoted stanza from the backlog, then insert the phase.
        self.backlog.drain(start..start + len);
        self.insert_phase(phase);
        Ok(phase_title)
    }

    /// Sweep every `## Backlog (not yet phased)` block out of the preamble AND
    /// out of node annotations, merging their bullet bodies into `self.backlog`
    /// (the canonical bottom section) with exact-line dedup. Returns the number
    /// of bullet lines newly swept in (0 = nothing to consolidate).
    ///
    /// Conservative: only the bridge-owned h2 heading matches
    /// ([is_backlog_heading]) — operator-curated sections like
    /// `### Backlog (rehomed from AA)` and `## Sustainment & minor features`
    /// are left exactly where they are.
    ///
    /// This is the explicit normalizer behind `canonicalize` and the
    /// backlog-mutating writeback paths. It is deliberately NOT called on a
    /// plain tick/rename, so an unrelated write never relocates a plan's
    /// Backlog (the trailing-block auto-lift in the parser is what keeps an
    /// already-canonical Backlog pinned to the bottom across ordinary writes).
    pub fn consolidate_backlog(&mut self) -> usize {
        let mut collected: Vec<String> = Vec::new();
        extract_backlog_from_preamble(&mut self.preamble, &mut collected);
        for phase in &mut self.phases {
            extract_backlog_from_annotation_list(&mut phase.annotations, &mut collected);
            for child in &mut phase.children {
                extract_backlog_from_annotations(child, &mut collected);
            }
        }
        let mut swept = 0;
        for line in collected {
            if !self.backlog.contains(&line) {
                self.backlog.push(line);
                swept += 1;
            }
        }
        swept
    }
}

/// Pull every backlog block (h1 `# Backlog (not yet phased)` or legacy h2
/// `## Backlog (not yet phased)`) out of `lines` (a preamble), pushing each
/// content line into `out`. A block runs from the heading to the next
/// markdown heading / `---` / EOF. Handles multiple sibling sections.
///
/// Phase 36.4: preserves indented continuation lines under bullets, not just
/// the bullets themselves — so a descoped subtree (`- X.1 - Foo\n  - X.1.1 -
/// Sub\n    Prose for the task`) round-trips intact through consolidation.
fn extract_backlog_from_preamble(lines: &mut Vec<String>, out: &mut Vec<String>) {
    loop {
        let Some(start) = lines.iter().position(|l| is_backlog_heading(l)) else {
            return;
        };
        let mut end = start + 1;
        while end < lines.len() {
            if looks_like_markdown_header(&lines[end]) || lines[end].trim() == "---" {
                break;
            }
            end += 1;
        }
        for line in &lines[start + 1..end] {
            let trimmed = line.trim_start();
            if trimmed.starts_with('-') {
                out.push(line.clone());
            } else if line.starts_with(char::is_whitespace) && !trimmed.is_empty() {
                // Indented continuation under a previous bullet — e.g. prose
                // under a descoped subtask. Preserve verbatim so nested
                // subtrees round-trip intact.
                out.push(line.clone());
            }
            // Else: blank line or stray column-0 prose — drop. The serializer
            // re-inserts exactly one blank between bullets.
        }
        lines.drain(start..end);
        // Collapse a now-doubled blank where the removed block sat, so repeated
        // consolidation doesn't grow vertical whitespace.
        if start > 0
            && start < lines.len()
            && lines[start - 1].trim().is_empty()
            && lines[start].trim().is_empty()
        {
            lines.remove(start);
        }
    }
}

/// Pull a `## Backlog (not yet phased)` block out of a node's annotations
/// (and, recursively, its children's). The block is a backlog-heading `Text`
/// annotation followed by the contiguous run of `Bullet` annotations; each
/// bullet is reconstructed as `- <text>` and pushed into `out`. Handles a
/// mid-document Backlog that the parser didn't auto-lift (trailing blocks are
/// already lifted into `plan.backlog` at parse time).
fn extract_backlog_from_annotations(node: &mut Node, out: &mut Vec<String>) {
    extract_backlog_from_annotation_list(&mut node.annotations, out);
    for child in &mut node.children {
        extract_backlog_from_annotations(child, out);
    }
}

/// Inner sweep: walk a single annotation list, pulling out backlog blocks.
/// Shared by [extract_backlog_from_annotations] (Node) and the Phase-level
/// walker used in [Plan::consolidate_backlog].
///
/// Phase 41.6: previously the walker only captured contiguous
/// `Annotation::Bullet`s after the heading AND stripped their indent on
/// the way out — both bugs surfaced by the quicksight upstream report
/// (2026-05-22), where a `## Backlog` block attached to a phase
/// contained nested bullets with indented prose continuations:
///
/// ```text
/// - Model-driven docs (drift reduction)
///   - X.10 — Runner: intra-cell layer DAG
///     The per-cell chain ...
///   - X.10.a cell_chain expresses deps
/// ```
///
/// The old logic broke on the first Text (prose continuation), stranding
/// every subsequent bullet/prose line in the source; and the bullets it
/// DID extract lost their indent. Result: 90 lines of orphaned markdown
/// between the last phase and the new `# Backlog` heading.
///
/// New rule: keep walking while each annotation is one of
///   * `Bullet { indent }` — emit `<indent>- <text>` preserving depth
///   * `Text { indent: n }` where `n > 0` — indented prose continuation
///     under a previous bullet; emit `<indent><text>` verbatim
///   * `Blank { count }` — preserve blank-line gaps between cluster
///     headlines (FORMATv2 backlogs are visually grouped this way)
///
/// Stop on column-0 `Text` (a new heading / top-level prose) or any
/// `CodeBlock` (deliberate boundary — fenced blocks inside a backlog
/// section are vanishingly rare and signal end-of-list).
fn extract_backlog_from_annotation_list(annotations: &mut Vec<Annotation>, out: &mut Vec<String>) {
    let mut i = 0;
    while i < annotations.len() {
        let is_heading = matches!(
            &annotations[i],
            Annotation::Text { text, .. } if is_backlog_heading(text)
        );
        if is_heading {
            let mut end = i + 1;
            while end < annotations.len() {
                match &annotations[end] {
                    Annotation::Bullet { text, indent } => {
                        out.push(format!("{}- {}", " ".repeat(*indent), text));
                        end += 1;
                    }
                    Annotation::Text { text, indent } if *indent > 0 => {
                        // Indented prose continuation under a previous
                        // bullet (e.g., the "The per-cell chain ..."
                        // detail under "- X.10 — Runner ..."). Preserve
                        // verbatim with its original indent.
                        out.push(format!("{}{}", " ".repeat(*indent), text));
                        end += 1;
                    }
                    Annotation::Blank { count } => {
                        for _ in 0..*count {
                            out.push(String::new());
                        }
                        end += 1;
                    }
                    // Column-0 Text or CodeBlock — boundary.
                    _ => break,
                }
            }
            annotations.drain(i..end);
            // Re-check at the same index (drain shifted things down).
        } else {
            i += 1;
        }
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

/// Phase variant of [remove_descendant]: searches a phase's task subtree for
/// an id and removes the matching Node.
fn remove_descendant_in_phase(phase: &mut Phase, id: &str) -> Option<Node> {
    if let Some(idx) = phase.children.iter().position(|c| c.id == id) {
        return Some(phase.children.remove(idx));
    }
    for child in &mut phase.children {
        if let Some(detached) = remove_descendant(child, id) {
            return Some(detached);
        }
    }
    None
}

/// Flatten a [Phase] back into a Node. Lossy — drops `depends_on`. Used by
/// callers that store a swept phase as a Node for re-serialization
/// (`Plan::remove`).
fn phase_to_node(phase: Phase) -> Node {
    Node {
        id: phase.id,
        title: phase.title,
        state: phase.state,
        children: phase.children,
        annotations: phase.annotations,
    }
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

/// Render a node + its subtree as plain (no-checkbox) backlog bullets at
/// `depth * 2`-space indent. Recursive — used by [Plan::append_backlog_subtree]
/// to preserve nested structure when descoping a subtree.
fn push_subtree_lines(out: &mut Vec<String>, node: &Node, depth: usize) {
    let indent = "  ".repeat(depth);
    let body = if node.title.is_empty() {
        node.id.clone()
    } else if node.id.is_empty() {
        node.title.clone()
    } else {
        format!("{} - {}", node.id, node.title)
    };
    out.push(format!("{indent}- {body}"));
    for child in &node.children {
        push_subtree_lines(out, child, depth + 1);
    }
}

/// Phase variant of [insert_in_order]: sort-insert a [Phase] into a slice
/// of phases by id.
fn insert_phase_in_order(phases: &mut Vec<Phase>, new_phase: Phase) {
    let pos = phases
        .iter()
        .position(|p| cmp_ids(&new_phase.id, &p.id) == std::cmp::Ordering::Less)
        .unwrap_or(phases.len());
    phases.insert(pos, new_phase);
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

/// Derive the parent id for a canonical plan_path. Returns None for a bare
/// top-level phase id (`42`, `AH`). The parent of any deeper id is that id
/// minus its last dot-segment: `42.1` -> `42`, `42.1.1` -> `42.1`.
///
/// Phase 42.3: phases are bare ids under FORMATv2; the legacy `.0` special-
/// case (phase `X.0`, parent-of-`X.1` == `X.0`) is gone.
pub fn parent_id_for(plan_path: &str) -> Option<String> {
    let parts: Vec<&str> = plan_path.split('.').collect();
    if parts.len() <= 1 {
        None
    } else {
        Some(parts[..parts.len() - 1].join("."))
    }
}

/// True when `s` is a well-formed plan id: only ASCII alphanumerics and dots,
/// with no empty dot-separated segments (`..`, `1.`, `.1` all rejected).
/// Mirrors the parser's `is_valid_id` character set but is exposed for callers
/// like the BY.11 description fallback that feed it arbitrary text (a
/// TaskCreate's `description`) rather than a pre-filtered token slice — prose
/// with spaces fails immediately.
pub fn is_valid_plan_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
        && s.split('.').all(|seg| !seg.is_empty())
}

/// Phase 42.3: phases are bare ids (`42`) under FORMATv2, but during migration
/// one may still be parsed/stored in the legacy `X.0` form. Treat a bare query
/// `X` as matching a phase whose id is `X` or the legacy `X.0`. Transitional —
/// once `canonicalize` has flipped every phase to the bare form this only ever
/// hits the exact-match arm.
fn phase_id_matches(phase_id: &str, query: &str) -> bool {
    phase_id == query || phase_id.strip_suffix(".0") == Some(query)
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
    /// One or more consecutive blank lines inside the phase tree. Round-trip
    /// preserves the count so vertical whitespace the user inserted between
    /// nodes survives.
    Blank { count: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_preserves_plan() {
        let plan = Plan {
            preamble: vec!["# Header".to_string(), "".to_string()],
            backlog: vec![],
            backlog_h1: false,
            phases: vec![Phase {
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
                depends_on: vec![],
                prefer_after: vec![],
            }],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let back: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, back);
    }

    #[test]
    fn parent_id_for_handles_canonical_shapes() {
        // Phase 42.3: bare phase ids have no parent; any deeper id drops its
        // last dot-segment. The legacy `.0` special-case is gone.
        assert_eq!(parent_id_for("1"), None);
        assert_eq!(parent_id_for("AH"), None);
        assert_eq!(parent_id_for("1.1"), Some("1".to_string()));
        assert_eq!(parent_id_for("AH.3"), Some("AH".to_string()));
        assert_eq!(parent_id_for("1.1.1"), Some("1.1".to_string()));
        assert_eq!(parent_id_for("X.4.a.1"), Some("X.4.a".to_string()));
    }

    #[test]
    fn find_walks_the_tree() {
        let plan = Plan {
            preamble: vec![],
            backlog: vec![],
            backlog_h1: false,
            phases: vec![Phase {
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
                depends_on: vec![],
                prefer_after: vec![],
            }],
        };
        // Phase 36: top-level phase ids resolve via find_phase / contains_id.
        assert!(plan.find_phase("1.0").is_some());
        assert!(plan.contains_id("1.0"));
        assert!(plan.find("1.1").is_some());
        assert!(plan.find("1.1.1").is_some());
        assert!(plan.find("1.2").is_none());
        assert!(plan.find("9.9").is_none());
        assert!(!plan.contains_id("9.9"));
    }

    #[test]
    fn add_child_of_appends() {
        let mut plan = Plan {
            preamble: vec![],
            backlog: vec![],
            backlog_h1: false,
            phases: vec![Phase {
                id: "1.0".to_string(),
                title: "Phase".to_string(),
                state: NodeState::Pending,
                children: vec![],
                annotations: vec![],
                depends_on: vec![],
                prefer_after: vec![],
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
            parse_for_test("## Phase 7 - Phase\n  - [ ] 7.1 a\n  - [ ] 7.2 b\n  - [ ] 7.3 c\n");
        let new_child = Node {
            id: "7.2a".to_string(),
            title: "between".to_string(),
            state: NodeState::Pending,
            children: vec![],
            annotations: vec![],
        };
        plan.add_child_of("7", new_child).unwrap();
        let parent = plan.find_phase("7").unwrap();
        let ids: Vec<&str> = parent.children.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["7.1", "7.2", "7.2a", "7.3"]);
    }

    #[test]
    fn add_child_of_prepends_when_new_id_is_smallest() {
        let mut plan = parse_for_test("## Phase 1 - Phase\n  - [ ] 1.5 mid\n  - [ ] 1.9 last\n");
        let new_child = Node {
            id: "1.1".to_string(),
            title: "first".to_string(),
            state: NodeState::Pending,
            children: vec![],
            annotations: vec![],
        };
        plan.add_child_of("1", new_child).unwrap();
        let ids: Vec<&str> = plan
            .find_phase("1")
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
        let mut plan = parse_for_test("## Phase 1 - a\n## Phase 3 - c\n");
        plan.insert_phase(Phase {
            id: "2".to_string(),
            title: "b".to_string(),
            state: NodeState::Pending,
            children: vec![],
            annotations: vec![],
            depends_on: vec![],
            prefer_after: vec![],
        });
        let ids: Vec<&str> = plan.phases.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["1", "2", "3"]);
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
    fn remove_pulls_a_leaf() {
        let mut plan = parse_for_test("## Phase 1 - Phase\n  - [ ] 1.1 Task\n");
        let removed = plan.remove("1.1").unwrap();
        assert_eq!(removed.id, "1.1");
        assert!(plan.find("1.1").is_none());
        assert!(plan.find_phase("1").is_some(), "parent should remain");
    }

    #[test]
    fn remove_pulls_a_top_level_phase() {
        let mut plan = parse_for_test("## Phase 1 - P1\n## Phase 2 - P2\n");
        plan.remove("1").unwrap();
        assert!(plan.find_phase("1").is_none());
        assert!(plan.find_phase("2").is_some());
    }

    #[test]
    fn remove_returns_none_when_missing() {
        let mut plan = parse_for_test("## Phase 1 - P\n");
        assert!(plan.remove("nope").is_none());
    }

    fn parse_for_test(input: &str) -> Plan {
        crate::parser::parse(input).unwrap()
    }

    #[test]
    fn append_backlog_note_dedups_exact_line() {
        let mut plan = Plan::default();
        plan.append_backlog_note("Try a thing", "2026-05-19");
        plan.append_backlog_note("Try a thing", "2026-05-19");
        assert_eq!(plan.backlog, vec!["- **Try a thing** — added 2026-05-19."]);
    }

    #[test]
    fn append_backlog_deferral_dedups_by_source_path() {
        let mut plan = Plan::default();
        plan.append_backlog_deferral("7.2", "Some task", "2026-05-19");
        // Same source path, different date — still a no-op.
        plan.append_backlog_deferral("7.2", "Some task", "2026-05-20");
        assert_eq!(
            plan.backlog,
            vec!["- **Some task** — deferred from 7.2 on 2026-05-19."]
        );
    }

    #[test]
    fn remove_backlog_note_drops_matching_title() {
        let mut plan = Plan::default();
        plan.append_backlog_note("Keep me", "2026-05-19");
        plan.append_backlog_note("Drop me", "2026-05-19");
        assert!(plan.remove_backlog_note("Drop me"));
        assert_eq!(plan.backlog, vec!["- **Keep me** — added 2026-05-19."]);
        assert!(!plan.remove_backlog_note("Not present"));
    }

    #[test]
    fn consolidate_sweeps_preamble_backlog_to_field() {
        let input = "\
# Title

## Backlog (not yet phased)

- **A** — added 2026-05-19.
- **B** — deferred from 1.2 on 2026-05-19.

## Phase 1 - Phase
";
        let mut plan = parse_for_test(input);
        assert!(plan.backlog.is_empty(), "preamble backlog not auto-lifted");
        let swept = plan.consolidate_backlog();
        assert_eq!(swept, 2);
        assert_eq!(
            plan.backlog,
            vec![
                "- **A** — added 2026-05-19.",
                "- **B** — deferred from 1.2 on 2026-05-19."
            ]
        );
        // Heading + bullets gone from the preamble.
        assert!(!plan.preamble.iter().any(|l| is_backlog_heading(l)));
        assert!(!plan.preamble.iter().any(|l| l.contains("**A**")));
    }

    #[test]
    fn consolidate_merges_duplicate_sections_and_dedups() {
        let input = "\
## Backlog (not yet phased)

- **Dup** — added 2026-05-19.

## Backlog (not yet phased)

- **Dup** — added 2026-05-19.
- **Unique** — added 2026-05-19.

## Phase 1 - Phase
";
        let mut plan = parse_for_test(input);
        plan.consolidate_backlog();
        assert_eq!(
            plan.backlog,
            vec![
                "- **Dup** — added 2026-05-19.",
                "- **Unique** — added 2026-05-19."
            ]
        );
    }

    #[test]
    fn consolidate_preserves_nested_bullets_and_prose_under_backlog_headline() {
        // Phase 41.6 regression: quicksight 2026-05-22 upstream report.
        // A `## Backlog (not yet phased)` block attached to a phase
        // (mid-document, not trailing — so trail-peel misses it) with
        // nested bullets + indented prose continuations under a headline.
        // Pre-41.6: the walker captured only the top-line bullet, broke
        // on the first indented prose, and stripped indent from the
        // bullets it did extract — stranding the children in source.
        // Post-41.6: every bullet + indented continuation + blank-line
        // gap survives the move with indent intact.
        let input = "\
## Phase 1 - Phase

## Backlog (not yet phased)

- Model-driven docs (drift reduction)
  - X.10 — Runner: intra-cell layer DAG
    The per-cell chain runs strictly serially today, but db / app2 /
    deploy only depend on seed_variant — they're true siblings.

  - X.10.a cell_chain expresses deps, not just order
  - X.10.b _run_one_variant gathers the sibling layers
- AA.A.10 (stretch) — Tree-walk picker→column derivation
  Even after AA.A.9, PickerSpec.column is still hand-mapped.

## Phase 2 - Next phase
";
        let mut plan = parse_for_test(input);
        plan.consolidate_backlog();
        let joined = plan.backlog.join("\n");

        // Headline + every nested child + the prose continuation all
        // land in plan.backlog with indent preserved.
        assert!(
            plan.backlog
                .iter()
                .any(|l| l == "- Model-driven docs (drift reduction)"),
            "headline present at column 0:\n{joined}"
        );
        assert!(
            plan.backlog
                .iter()
                .any(|l| l == "  - X.10 — Runner: intra-cell layer DAG"),
            "X.10 nested child preserved at indent=2:\n{joined}"
        );
        assert!(
            plan.backlog
                .iter()
                .any(|l| l.contains("per-cell chain runs")),
            "indented prose continuation NOT stranded:\n{joined}"
        );
        assert!(
            plan.backlog
                .iter()
                .any(|l| l == "  - X.10.a cell_chain expresses deps, not just order"),
            "X.10.a preserved at indent=2:\n{joined}"
        );
        assert!(
            plan.backlog
                .iter()
                .any(|l| l == "- AA.A.10 (stretch) — Tree-walk picker→column derivation"),
            "second cluster headline preserved:\n{joined}"
        );
        assert!(
            plan.backlog
                .iter()
                .any(|l| l.contains("AA.A.9, PickerSpec.column")),
            "second cluster's prose preserved:\n{joined}"
        );
    }

    #[test]
    fn consolidate_leaves_h3_and_sustainment_untouched() {
        let input = "\
## Phase 1 - Phase

### Backlog (rehomed from AA)

- **Rehomed item** — keep me here.

## Sustainment & minor features

- **Sustainment item** — keep me too.
";
        let mut plan = parse_for_test(input);
        let swept = plan.consolidate_backlog();
        assert_eq!(swept, 0, "neither operator section is bridge-owned");
        assert!(plan.backlog.is_empty());
    }

    #[test]
    fn is_backlog_heading_excludes_h3_and_siblings() {
        assert!(is_backlog_heading("## Backlog (not yet phased)"));
        assert!(is_backlog_heading("## Backlog"));
        // h3 subsection is operator-curated — must not match.
        assert!(!is_backlog_heading("### Backlog (rehomed from AA)"));
        // Unrelated h2 sibling.
        assert!(!is_backlog_heading("## Sustainment & minor features"));
    }

    #[test]
    fn deferral_consolidates_preamble_backlog_to_bottom() {
        // A legacy preamble Backlog merges down to the bottom field rather than
        // splitting into two sections when a new deferral lands.
        let mut plan = parse_for_test(
            "# Title\n\n## Backlog (not yet phased)\n\n- **Existing item** — context.\n\n## Phase 1 - Phase\n",
        );
        plan.consolidate_backlog();
        plan.append_backlog_deferral("28.7", "Test entry", "2026-05-17");
        let serialized = crate::serializer::serialize(&plan);
        assert_eq!(serialized.matches("## Backlog (not yet phased)").count(), 1);
        assert!(serialized.contains("- **Existing item** — context."));
        assert!(serialized.contains("- **Test entry** — deferred from 28.7 on 2026-05-17."));
        // Backlog renders below the phase.
        assert!(
            serialized.find("## Backlog").unwrap() > serialized.find("## Phase 1 - Phase").unwrap()
        );
    }

    #[test]
    fn deferral_creates_section_when_missing() {
        let mut plan = parse_for_test("# Title\n\n## Phase 1 - Phase\n");
        plan.append_backlog_deferral("28.7", "Bootstrap entry", "2026-05-17");
        let serialized = crate::serializer::serialize(&plan);
        assert!(serialized.contains("## Backlog (not yet phased)"));
        assert!(serialized.contains("- **Bootstrap entry** — deferred from 28.7 on 2026-05-17."));
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

    // ---- Phase CE.3.3: Plan::breakdown ----

    #[test]
    fn breakdown_appends_numbered_children_under_a_task() {
        let mut plan = parse_for_test("## Phase CE - x\n- [ ] CE.3 - Implement\n");
        let added = plan
            .breakdown("CE.3", &["codec".to_string(), "scan".to_string()])
            .unwrap();
        assert_eq!(added, vec!["CE.3.1", "CE.3.2"]);
        let node = plan.find("CE.3").unwrap();
        assert_eq!(node.children.len(), 2);
        assert_eq!(node.children[0].title, "codec");
    }

    #[test]
    fn breakdown_is_recursive_and_appends_repeatedly() {
        let mut plan = parse_for_test("## Phase CE - x\n- [ ] CE.3 - Implement\n");
        plan.breakdown("CE.3", &["a".to_string(), "b".to_string()])
            .unwrap();
        // Recursive: break down a child at the next depth.
        assert_eq!(
            plan.breakdown("CE.3.2", &["deep".to_string()]).unwrap(),
            vec!["CE.3.2.1"]
        );
        // Repeatable: appends after the highest existing suffix.
        assert_eq!(
            plan.breakdown("CE.3", &["c".to_string()]).unwrap(),
            vec!["CE.3.3"]
        );
    }

    #[test]
    fn breakdown_works_on_a_phase_id() {
        let mut plan = parse_for_test("## Phase CE - x\n- [ ] CE.1 - First\n");
        assert_eq!(
            plan.breakdown("CE", &["second".to_string()]).unwrap(),
            vec!["CE.2"]
        );
    }

    #[test]
    fn breakdown_errors_on_unknown_parent_and_empty_subjects() {
        let mut plan = parse_for_test("## Phase CE - x\n- [ ] CE.1 - First\n");
        assert!(plan.breakdown("ZZ.9", &["x".to_string()]).is_err());
        assert!(
            plan.breakdown("CE.1", &["".to_string(), "  ".to_string()])
                .is_err()
        );
    }

    #[test]
    fn cg_backlog_entries_group_by_top_level_bullet() {
        let plan = parse_for_test(
            "## Phase A - x\n- [ ] A.1 - t\n\n# Backlog (not yet phased)\n\n- **Auth** — rotate keys\n  - login flow\n  - admin panel\n- **Drop fs4** — std lock\n",
        );
        let entries = plan.backlog_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].headline, "Auth");
        assert_eq!(
            entries[0].detail,
            vec!["rotate keys", "  - login flow", "  - admin panel"]
        );
        assert_eq!(entries[1].headline, "Drop fs4");
        assert_eq!(entries[1].detail, vec!["std lock"]);
    }

    #[test]
    fn cg_split_headline_variants() {
        let plan = parse_for_test(
            "## Phase A - x\n\n# Backlog (not yet phased)\n\n- **Bold lead** — detail tail\n- Plain headline — em dash detail\n- Just a plain bullet\n",
        );
        let e = plan.backlog_entries();
        assert_eq!(e[0].headline, "Bold lead");
        assert_eq!(e[0].detail, vec!["detail tail"]);
        assert_eq!(e[1].headline, "Plain headline");
        assert_eq!(e[1].detail, vec!["em dash detail"]);
        assert_eq!(e[2].headline, "Just a plain bullet");
        assert!(e[2].detail.is_empty());
    }

    #[test]
    fn cg_promote_builds_phase_with_prose_and_removes_entry() {
        let mut plan = parse_for_test(
            "## Phase A - x\n- [ ] A.1 - t\n\n# Backlog (not yet phased)\n\n- **Auth** — rotate keys\n  - login flow\n- **Drop fs4** — std lock\n",
        );
        let title = plan.promote_backlog_entry(1, None, "B").unwrap();
        assert_eq!(title, "Auth");
        let b = plan.find_phase("B").expect("phase B exists");
        assert_eq!(b.title, "Auth");
        // Detail became phase-level prose, NOT tasks.
        assert!(b.children.is_empty(), "should have no tasks");
        assert!(!b.annotations.is_empty(), "detail became annotations");
        // The promoted entry is gone; only the fs4 entry remains.
        let left = plan.backlog_entries();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].headline, "Drop fs4");
    }

    #[test]
    fn cg_promote_title_override_and_out_of_range() {
        let mut plan =
            parse_for_test("## Phase A - x\n\n# Backlog (not yet phased)\n\n- **Auth** — keys\n");
        assert!(plan.clone().promote_backlog_entry(2, None, "B").is_err());
        assert!(plan.clone().promote_backlog_entry(0, None, "B").is_err());
        let title = plan
            .promote_backlog_entry(1, Some("Custom Title"), "B")
            .unwrap();
        assert_eq!(title, "Custom Title");
        assert_eq!(plan.find_phase("B").unwrap().title, "Custom Title");
    }
}

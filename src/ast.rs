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
/// The `state`/`id_style`/`separator` fields are v1 holdovers — they describe
/// the legacy `- [ ] N.0` anchor form. Once Phase 37 lands the FORMATv2
/// serializer they become advisory only (and a future phase will drop them
/// from the on-disk representation entirely).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub state: NodeState,
    #[serde(default)]
    pub id_style: IdStyle,
    #[serde(default)]
    pub separator: Separator,
    /// Tasks under the phase. Field is named `children` (rather than `tasks`)
    /// for 36.1 so consumers that read `phase.children` keep compiling without
    /// a rename sweep; 36.2 will tighten the naming.
    #[serde(default)]
    pub children: Vec<Node>,
    /// Annotations attached at the phase level. Today this is everything the
    /// parser used to hang off the v1 `N.0` anchor; 36.5 will split out
    /// phase-level prose (lines under a `## Phase` header not attached to any
    /// task) as a distinct bucket.
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
    /// Tracks how this phase appeared on disk so the serializer can preserve
    /// the format on routine writes. v1 `- [ ] N.0 Title` anchors stay as
    /// anchors; v2 `## Phase X - Title` headers stay as headers. Explicit
    /// canonicalize flips every phase to `HeaderV2` for a one-shot
    /// migration. Default `LegacyAnchor` keeps backward compatibility for
    /// state-file deserialization and bridge-internal Phase construction.
    #[serde(default)]
    pub source: PhaseSource,
}

/// Origin format of a [Phase] — controls serializer dispatch. New phases
/// parsed from a v1 `- [ ] N.0` anchor (and Phases created via the legacy
/// path before 37) are [PhaseSource::LegacyAnchor]; phases parsed from a v2
/// `## Phase X - Title` header are [PhaseSource::HeaderV2].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PhaseSource {
    #[default]
    LegacyAnchor,
    HeaderV2,
}

/// A single checkbox node in the plan. Tasks and subtasks share this shape;
/// depth is determined by the dotted `id` (e.g., `1.1`, `1.1.1`) and by tree
/// position. Top-level phases use [Phase] instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub state: NodeState,
    /// Presentation-only: whether the id was bold-wrapped (`**1.2.3**`) in
    /// source. Round-trip preserves this; `standardize_to_canonical` flattens
    /// to `Plain`.
    #[serde(default)]
    pub id_style: IdStyle,
    /// Presentation-only: separator between id and title in source. Round-trip
    /// preserves this; canonical form is `Space`.
    #[serde(default)]
    pub separator: Separator,
    pub children: Vec<Node>,
    pub annotations: Vec<Annotation>,
}

/// Whether the id was bold-wrapped (`**1.2.3**`) in the source. Round-trip
/// preserves this; canonical form is `Plain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdStyle {
    #[default]
    Plain,
    Bold,
}

/// Separator between id and title in the source line. Round-trip preserves
/// this; canonical form is `Space`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Separator {
    #[default]
    Space,
    EmDash,
    Hyphen,
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

    /// Build a Phase from a top-level Node — used by the parser when it
    /// finishes assembling a `- [ ] N.0 ...` anchor and promotes it to the
    /// phase tier, and by anywhere that wants to wrap a legacy-anchor-shaped
    /// node into a Phase for insertion.
    pub fn from_node(node: Node) -> Self {
        Self {
            id: node.id,
            title: node.title,
            state: node.state,
            id_style: node.id_style,
            separator: node.separator,
            children: node.children,
            annotations: node.annotations,
            depends_on: Vec::new(),
            prefer_after: Vec::new(),
            source: PhaseSource::LegacyAnchor,
        }
    }

    /// True when the serializer should emit this phase as a FORMATv2
    /// `## Phase X - Title` header rather than a v1 `- [ ] N.0` anchor.
    /// Driven by `Phase::source`; canonicalize is the explicit operation
    /// that flips legacy anchors to header form.
    pub fn is_v2_header_form(&self) -> bool {
        matches!(self.source, PhaseSource::HeaderV2)
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
///
/// If the subtree contains MORE than one Phase-N header, we bail without
/// stripping anything — promotion would be ambiguous (which header bounds
/// which top-level phase?). The headers stay as `Annotation::Text` and the
/// serializer (which respects their original indent for markdown headers)
/// preserves them verbatim. The user sees no refusal and no demotion; the
/// only thing they lose is auto-promotion of those particular headers.
fn strip_and_collect_phase_headers(
    node: &mut Node,
    out: &mut Vec<(String, String)>,
    conversions: &mut Vec<String>,
) {
    if count_phase_headers_in_subtree(node) > 1 {
        return;
    }
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

fn flatten_id_style(node: &mut Node) {
    node.id_style = IdStyle::Plain;
    node.separator = Separator::Space;
    for child in &mut node.children {
        flatten_id_style(child);
    }
}

fn count_phase_headers_in_subtree(node: &Node) -> usize {
    let here = node
        .annotations
        .iter()
        .filter(
            |a| matches!(a, Annotation::Text { text, .. } if parse_phase_header(text).is_some()),
        )
        .count();
    here + node
        .children
        .iter()
        .map(count_phase_headers_in_subtree)
        .sum::<usize>()
}

fn flush_phase_group(
    out: &mut Vec<Phase>,
    pending: &mut Vec<Node>,
    current_header: &mut Option<(String, String)>,
) {
    if pending.is_empty() {
        return;
    }
    if let Some((id, title)) = current_header.take() {
        let children: Vec<Node> = std::mem::take(pending);
        out.push(Phase {
            id,
            title,
            state: NodeState::Pending,
            id_style: IdStyle::Plain,
            separator: Separator::Space,
            children,
            annotations: vec![],
            depends_on: vec![],
            prefer_after: vec![],
            // Standardize-promoted phases (from legacy `### Phase N — Title`
            // markdown headers) land in HeaderV2 form so the next write emits
            // them as `## Phase N - Title`.
            source: PhaseSource::HeaderV2,
        });
    } else {
        // Promote every orphan top-level Node (no captured Phase header
        // wrapping it) into its own Phase. Legacy `- [ ] N.0` anchors land
        // here — their state/style/separator come along for v1 round-trip.
        for node in pending.drain(..) {
            out.push(Phase::from_node(node));
        }
    }
}

impl Plan {
    /// Every leaf across all phases, returned as a uniform Phase-or-Node view.
    /// A *phase* qualifies as a leaf when it has no tasks under it (legacy v1
    /// `- [ ] N.0 Foo` with no children — the anchor itself was the unit of
    /// work). A *task* qualifies as a leaf when it has no nested children.
    /// Document order: childless phase emits its own item; non-empty phase
    /// emits each leaf descendant of its task subtree.
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
        self.phases.iter().find(|p| p.id == id)
    }

    pub fn find_phase_mut(&mut self, id: &str) -> Option<&mut Phase> {
        self.phases.iter_mut().find(|p| p.id == id)
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
        if let Some(idx) = self.phases.iter().position(|p| p.id == id) {
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
        if let Some(phase) = self.phases.iter_mut().find(|p| p.id == parent_id) {
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

    /// Insert a top-level phase in id-sort order against existing phases.
    pub fn insert_phase(&mut self, phase: Phase) {
        insert_phase_in_order(&mut self.phases, phase);
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

        // Convert every existing Phase back into its Node-shaped form so the
        // header-stripping + promotion logic — which walks Node subtrees —
        // can operate uniformly. Phases that lose their header annotations
        // get rebuilt into Phases at the end via flush_phase_group.
        let mut tagged: Vec<(Vec<(String, String)>, Node)> = Vec::new();
        let mut conversions: Vec<String> = Vec::new();
        for phase in self.phases {
            let mut as_node = phase_to_node(phase);
            let mut headers_in_subtree: Vec<(String, String)> = Vec::new();
            strip_and_collect_phase_headers(
                &mut as_node,
                &mut headers_in_subtree,
                &mut conversions,
            );
            tagged.push((headers_in_subtree, as_node));
        }

        // Third pass: each phase's outgoing header is the single Phase-N
        // header that was in its subtree (if any). Multi-header subtrees were
        // skipped above — their headers stay as narrative annotations.
        let mut new_phases: Vec<Phase> = Vec::new();
        let mut pending: Vec<Node> = Vec::new();
        let mut current_header: Option<(String, String)> = None;
        for (mut headers, phase) in tagged {
            pending.push(phase);
            if let Some((id, title)) = headers.pop() {
                flush_phase_group(&mut new_phases, &mut pending, &mut current_header);
                current_header = Some((id, title));
            }
        }
        flush_phase_group(&mut new_phases, &mut pending, &mut current_header);

        // Phase 29.4: canonical form strips bold-wrapped IDs. The standardize
        // pass owns the destructive normalization; round-trip writebacks
        // preserve `IdStyle::Bold`.
        for phase in &mut new_phases {
            phase.id_style = IdStyle::Plain;
            phase.separator = Separator::Space;
            for child in &mut phase.children {
                flatten_id_style(child);
            }
        }

        Ok((
            Plan {
                preamble: self.preamble,
                phases: new_phases,
                backlog: self.backlog,
                backlog_h1: self.backlog_h1,
            },
            conversions,
        ))
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
    pub fn append_backlog_subtree(
        &mut self,
        node: &Node,
        source_phase: &str,
        date: &str,
    ) {
        // Idempotency probe: match on the deferral marker for this plan_path
        // at the top level (avoids double-add if backlog already had it).
        let top_marker = format!("(deferred from phase `{source_phase}` on");
        let id_marker = format!("- {} -", node.id);
        let already = self.backlog.iter().any(|line| {
            line.trim_start().starts_with(&id_marker) && line.contains(&top_marker)
        });
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
                if let Annotation::Bullet { text, .. } = &annotations[end] {
                    out.push(format!("- {text}"));
                    end += 1;
                } else {
                    break;
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
/// (`Plan::remove`), and internally by `standardize_to_canonical` so the
/// header-promotion logic can keep operating on Node subtrees.
fn phase_to_node(phase: Phase) -> Node {
    Node {
        id: phase.id,
        title: phase.title,
        state: phase.state,
        id_style: phase.id_style,
        separator: phase.separator,
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

/// Derive the parent id for a canonical plan_path. Returns None for top-level
/// (e.g. `1.0`, `AH.0`). For 2-part non-`.0` ids like `1.1` the parent is
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
                id_style: IdStyle::Plain,
                separator: Separator::Space,
                annotations: vec![Annotation::Text {
                    text: "note".to_string(),
                    indent: 2,
                }],
                children: vec![Node {
                    id: "1.1".to_string(),
                    title: "Task".to_string(),
                    state: NodeState::Done,
                    id_style: IdStyle::Plain,
                    separator: Separator::Space,
                    children: vec![],
                    annotations: vec![],
                }],
                depends_on: vec![],
                prefer_after: vec![],
            source: PhaseSource::LegacyAnchor,
            }],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let back: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, back);
    }

    #[test]
    fn parent_id_for_handles_canonical_shapes() {
        assert_eq!(parent_id_for("1.0"), None);
        assert_eq!(parent_id_for("AH.0"), None);
        assert_eq!(parent_id_for("1"), None);
        assert_eq!(parent_id_for("1.1"), Some("1.0".to_string()));
        assert_eq!(parent_id_for("AH.3"), Some("AH.0".to_string()));
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
                id_style: IdStyle::Plain,
                separator: Separator::Space,
                children: vec![Node {
                    id: "1.1".to_string(),
                    title: "Task".to_string(),
                    state: NodeState::Pending,
                    id_style: IdStyle::Plain,
                    separator: Separator::Space,
                    children: vec![Node {
                        id: "1.1.1".to_string(),
                        title: "Sub".to_string(),
                        state: NodeState::Done,
                        id_style: IdStyle::Plain,
                        separator: Separator::Space,
                        children: vec![],
                        annotations: vec![],
                    }],
                    annotations: vec![],
                }],
                annotations: vec![],
                depends_on: vec![],
                prefer_after: vec![],
            source: PhaseSource::LegacyAnchor,
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
                id_style: IdStyle::Plain,
                separator: Separator::Space,
                children: vec![],
                annotations: vec![],
                depends_on: vec![],
                prefer_after: vec![],
            source: PhaseSource::LegacyAnchor,
            }],
        };
        let child = Node {
            id: "1.1".to_string(),
            title: "Task".to_string(),
            state: NodeState::Pending,
            id_style: IdStyle::Plain,
            separator: Separator::Space,
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
            id_style: IdStyle::Plain,
            separator: Separator::Space,
            children: vec![],
            annotations: vec![],
        };
        plan.add_child_of("7.0", new_child).unwrap();
        let parent = plan.find_phase("7.0").unwrap();
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
            id_style: IdStyle::Plain,
            separator: Separator::Space,
            children: vec![],
            annotations: vec![],
        };
        plan.add_child_of("1.0", new_child).unwrap();
        let ids: Vec<&str> = plan
            .find_phase("1.0")
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
        plan.insert_phase(Phase {
            id: "2.0".to_string(),
            title: "b".to_string(),
            state: NodeState::Pending,
            id_style: IdStyle::Plain,
            separator: Separator::Space,
            children: vec![],
            annotations: vec![],
            depends_on: vec![],
            prefer_after: vec![],
            source: PhaseSource::LegacyAnchor,
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
            id_style: IdStyle::Plain,
            separator: Separator::Space,
            children: vec![],
            annotations: vec![],
        };
        let err = plan.add_child_of("nope", child).unwrap_err();
        assert!(err.contains("nope"));
    }

    #[test]
    fn remove_pulls_a_leaf() {
        let mut plan = parse_for_test("- [ ] 1.0 Phase\n  - [ ] 1.1 Task\n");
        let removed = plan.remove("1.1").unwrap();
        assert_eq!(removed.id, "1.1");
        assert!(plan.find("1.1").is_none());
        assert!(plan.find_phase("1.0").is_some(), "parent should remain");
    }

    #[test]
    fn remove_pulls_a_top_level_phase() {
        let mut plan = parse_for_test("- [ ] 1.0 P1\n- [ ] 2.0 P2\n");
        plan.remove("1.0").unwrap();
        assert!(plan.find_phase("1.0").is_none());
        assert!(plan.find_phase("2.0").is_some());
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

- [ ] 1.0 Phase
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

- [ ] 1.0 Phase
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
    fn consolidate_leaves_h3_and_sustainment_untouched() {
        let input = "\
- [ ] 1.0 Phase

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
    fn standardize_leaves_multi_header_subtree_alone() {
        // Phase 20 regression — quicksight shakeout v0.1.6. When multiple
        // `## Phase N — Title` headers attach to the same top-level phase's
        // subtree (because intervening content didn't pop the parser stack),
        // standardize should leave them ALL as narrative rather than refuse.
        //
        // Phase 36.3 update: `## Phase X - Title` (and the em-dash variant)
        // is now a first-class FORMATv2 phase boundary at PARSE time — each
        // header opens its own Phase. The "multi-header subtree" case the
        // original test guarded is no longer possible: the parser produces
        // [0.1, X, AA, Z] phases directly. `standardize_to_canonical` is a
        // no-op on this input because every header already became a phase.
        let plan = parse_for_test(
            "- [ ] 0.1 First\n\n## Phase X — Top\n## Phase AA — Other\n## Phase Z — Third\n",
        );
        let pre_ids: Vec<String> = plan.phases.iter().map(|n| n.id.clone()).collect();
        assert_eq!(
            pre_ids,
            vec!["0.1", "X", "AA", "Z"],
            "v2 parser opens a phase per header at parse time"
        );

        let (out, _) = plan
            .standardize_to_canonical()
            .expect("standardize is a no-op when headers already parsed as phases");
        let post_ids: Vec<&str> = out.phases.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(post_ids, vec!["0.1", "X", "AA", "Z"]);
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
            backlog: vec![],
            backlog_h1: false,
            phases: vec![Phase {
                id: "1.0".to_string(),
                title: "Phase".to_string(),
                state: NodeState::Pending,
                id_style: IdStyle::Plain,
                separator: Separator::Space,
                children: vec![],
                annotations: vec![Annotation::Text {
                    text: "#hashtag style not a header".to_string(),
                    indent: 2,
                }],
                depends_on: vec![],
                prefer_after: vec![],
            source: PhaseSource::LegacyAnchor,
            }],
        };
        let (_, notes) = plan.standardize_to_canonical().unwrap();
        assert!(notes.is_empty());
    }

    #[test]
    fn deferral_consolidates_preamble_backlog_to_bottom() {
        // A legacy preamble Backlog merges down to the bottom field rather than
        // splitting into two sections when a new deferral lands.
        let mut plan = parse_for_test(
            "# Title\n\n## Backlog (not yet phased)\n\n- **Existing item** — context.\n\n- [ ] 1.0 Phase\n",
        );
        plan.consolidate_backlog();
        plan.append_backlog_deferral("28.7", "Test entry", "2026-05-17");
        let serialized = crate::serializer::serialize(&plan);
        assert_eq!(serialized.matches("## Backlog (not yet phased)").count(), 1);
        assert!(serialized.contains("- **Existing item** — context."));
        assert!(serialized.contains("- **Test entry** — deferred from 28.7 on 2026-05-17."));
        // Backlog renders below the phase.
        assert!(
            serialized.find("## Backlog").unwrap() > serialized.find("- [ ] 1.0 Phase").unwrap()
        );
    }

    #[test]
    fn deferral_creates_section_when_missing() {
        let mut plan = parse_for_test("# Title\n\n- [ ] 1.0 Phase\n");
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
}

//! Phase BZ: the phase-name sequence. Phase ids are uppercase-letter tokens
//! incrementing in bijective base-26 (spreadsheet-column order):
//! `A..Z` -> `AA..AZ` -> `BA..BZ` -> ... -> `ZZ` -> `AAA` -> ...
//!
//! This is *bijective* base-26 (no zero digit): `A` is the first id, `Z` rolls
//! over to `AA` (not to a leading-zero two-letter form). The operations work
//! directly on the string — an odometer-style increment and a (length, then
//! lexicographic) comparison. This module is the single source of truth for
//! the ordering; [`next_phase_id`] (BZ.2) and the high-water-mark scan (BZ.3)
//! build on it.
//!
//! Uppercase A-Z only, by deliberate project policy: numeric phase ids (`1`,
//! `42`) are legacy and are not part of this sequence — they fail
//! [`is_alpha_phase_id`] and are ignored when reconstructing the latest id.
//!
//! Phase CJ: ids are also **length-capped** at [`MAX_PHASE_ID_LEN`]. The
//! odometer still increments strings of any length mechanically, but a token
//! longer than the cap is NOT a member of the sequence — it fails
//! [`is_sequence_phase_id`], so [`scan_sequence_phase_ids`] skips it when
//! reconstructing the high-water mark. That is the guard against garbled /
//! concatenated headers (`## Phase CICJ`): without a cap, one such token —
//! ranked highest by the length-first ordering — would poison the sequence and
//! every id thereafter would be 4+ letters, permanently. The cap (3 letters =
//! 18,278 ids) is far past any real plan, so a token that exceeds it is
//! garbage, not a phase. [`next_phase_id`] returns `None` rather than mint an
//! over-cap id.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// The most letters a phase id may have and still count as a member of the
/// sequence (Phase CJ). Three letters is 26³ + 26² + 26 = 18,278 distinct ids —
/// orders of magnitude past any real plan (this repo is at ~90 phases). A token
/// longer than this is treated as garbage (a garbled / concatenated header),
/// not a phase id: see [`is_sequence_phase_id`].
pub const MAX_PHASE_ID_LEN: usize = 3;

/// True when `s` is a syntactically well-formed alpha id: a non-empty run of
/// uppercase ASCII letters and nothing else. Rejects the empty string,
/// lowercase, digits, dots, spaces, and non-ASCII. This is the *shape* check
/// only — it says nothing about length, so it still separates alpha ids from
/// legacy *numeric* ones (`42`) regardless of the cap. For the cap-aware
/// membership test the bridge hands out and counts, use [`is_sequence_phase_id`].
pub fn is_alpha_phase_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_uppercase())
}

/// True when `s` is a valid, in-bounds member of the phase-name sequence: a
/// well-formed alpha id ([`is_alpha_phase_id`]) that is also within
/// [`MAX_PHASE_ID_LEN`]. This is the predicate for "an id the bridge will hand
/// out or count toward the high-water mark" — an over-cap all-caps token
/// (`CICJ`) is well-formed alpha but NOT a sequence member, so it is excluded.
pub fn is_sequence_phase_id(s: &str) -> bool {
    is_alpha_phase_id(s) && s.len() <= MAX_PHASE_ID_LEN
}

/// The `key=value` payload of the phase high-water marker comment (Phase CJ).
/// The marker persists the high-water mark IN PLAN.md so next-id derivation no
/// longer has to read PLAN_ARCHIVE.md — an archived phase's id survives in the
/// marker even after the phase itself is swept out of PLAN.md.
pub const PHASE_HIGH_WATER_KEY: &str = "plan-bridge:phase-high-water";

/// Render the phase high-water marker as an HTML comment line (no trailing
/// newline). Invisible in rendered markdown, so it doesn't clutter the plan.
pub fn render_high_water_marker(id: &str) -> String {
    format!("<!-- {PHASE_HIGH_WATER_KEY}={id} -->")
}

/// If `line` is a phase high-water marker comment, return its raw value token
/// (still unvalidated — the caller decides whether it's a usable sequence id).
/// Tolerant of surrounding whitespace inside the comment. Returns `None` for
/// any non-marker line.
pub fn parse_high_water_marker(line: &str) -> Option<&str> {
    let inner = line
        .trim()
        .strip_prefix("<!--")?
        .strip_suffix("-->")?
        .trim();
    let val = inner
        .strip_prefix(PHASE_HIGH_WATER_KEY)?
        .trim()
        .strip_prefix('=')?
        .trim();
    Some(val)
}

/// The successor of a phase id in the sequence: `A -> B`, `Z -> AA`,
/// `AZ -> BA`, `ZZ -> AAA`, `BY -> BZ`. Implemented as an odometer with carry —
/// a run of trailing `Z`s rolls to `A`s and grows the id by one digit when the
/// carry runs off the front.
///
/// Returns `None` in two cases, so it never hands back an id outside the
/// sequence:
///   - `current` is not a valid in-bounds sequence id ([`is_sequence_phase_id`])
///     — a legacy numeric id, or an over-cap garbled token. Neither has a
///     defined successor here.
///   - the successor would exceed [`MAX_PHASE_ID_LEN`] (i.e. `current` is
///     `Z`×cap, the last id in the namespace). That is genuine exhaustion —
///     18,278 phases at cap 3 — and the caller must treat it as an error, not
///     silently mint a longer id.
pub fn next_phase_id(current: &str) -> Option<String> {
    if !is_sequence_phase_id(current) {
        return None;
    }
    let mut bytes = current.as_bytes().to_vec();
    let mut i = bytes.len();
    loop {
        if i == 0 {
            // Carry ran off the front: every digit was `Z`. The id grows by
            // one — `Z -> AA`, `ZZ -> AAA` — with all digits now `A`.
            bytes.insert(0, b'A');
            break;
        }
        i -= 1;
        if bytes[i] == b'Z' {
            bytes[i] = b'A';
            // carry continues left
        } else {
            bytes[i] += 1;
            break;
        }
    }
    if bytes.len() > MAX_PHASE_ID_LEN {
        // Namespace exhausted: the carry grew the id past the cap. Refuse
        // rather than mint an over-cap id.
        return None;
    }
    Some(String::from_utf8(bytes).expect("ASCII A-Z"))
}

/// Order two phase ids by their position in the sequence. For valid alpha ids
/// this is *length first, then lexicographic* — a longer id is always later
/// (`Z` < `AA`), and same-length ids compare letter-by-letter (`BY` < `BZ`).
/// That equals numeric order in bijective base-26 without ever building a
/// number, so it holds for ids of any length.
pub fn cmp_phase_ids(a: &str, b: &str) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Extract the in-bounds sequence phase ids from one markdown document by
/// matching FORMATv2 phase headers (`## Phase <id> - <title>`, at any heading
/// level). The id is the first whitespace-delimited token after `Phase`; only
/// ids that pass [`is_sequence_phase_id`] are yielded, so legacy numeric
/// (`## Phase 42`), dotted, AND over-cap garbled (`## Phase CICJ`) tokens are
/// skipped — none are part of the sequence, and skipping the garbled ones is
/// what stops a concatenated header from poisoning the high-water mark.
fn scan_sequence_phase_ids(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter_map(|line| {
        let after_hashes = line.trim_start().trim_start_matches('#');
        // Require at least one `#` was stripped AND a space separated it.
        if after_hashes.len() == line.trim_start().len() {
            return None;
        }
        let id = after_hashes.trim_start().strip_prefix("Phase ")?;
        let id = id.split_whitespace().next()?;
        is_sequence_phase_id(id).then_some(id)
    })
}

/// The highest in-bounds sequence phase id appearing as a `## Phase` header in
/// either document, or `None` when neither has one. This is the raw high-water
/// mark (not its successor) — [`next_phase_id_from_texts`] and the marker-aware
/// derivation both build on it.
pub fn high_water_of_texts(plan_text: &str, archive_text: &str) -> Option<String> {
    let mut best: Option<&str> = None;
    for text in [plan_text, archive_text] {
        for id in scan_sequence_phase_ids(text) {
            if best.is_none_or(|b| cmp_phase_ids(id, b) == Ordering::Greater) {
                best = Some(id);
            }
        }
    }
    best.map(str::to_string)
}

/// The highest valid phase high-water MARKER (`<!-- plan-bridge:phase-high-water
/// =XX -->`) in `text`, or `None` when the text carries no usable marker. A
/// garbled / over-cap marker value is ignored. Multiple markers (shouldn't
/// happen) collapse to the highest.
pub fn marker_of_text(text: &str) -> Option<String> {
    text.lines()
        .filter_map(parse_high_water_marker)
        .filter(|v| is_sequence_phase_id(v))
        .max_by(|a, b| cmp_phase_ids(a, b))
        .map(str::to_string)
}

/// Turn a high-water mark into the next id to hand out: its successor, or — when
/// the namespace is exhausted (`hw` is `Z`×cap, ~18k phases) — `hw` itself, so
/// the caller's "phase already exists" collision guard fires loudly instead of
/// silently resetting to `A`. `None` yields `"A"` (brand-new project).
fn next_after(hw: Option<String>) -> String {
    match hw {
        None => "A".to_string(),
        Some(b) => next_phase_id(&b).unwrap_or(b),
    }
}

/// Reconstruct the next phase id from raw plan + archive text (no marker). Kept
/// for the markerless / migration path and for tests; [`next_phase_id_for_plan`]
/// prefers the marker and skips the archive entirely once one exists.
pub fn next_phase_id_from_texts(plan_text: &str, archive_text: &str) -> String {
    next_after(high_water_of_texts(plan_text, archive_text))
}

/// Sibling `PLAN_ARCHIVE.md` next to a `PLAN.md` path.
pub fn archive_path_for(plan_path: &Path) -> PathBuf {
    plan_path.with_file_name("PLAN_ARCHIVE.md")
}

/// The next phase id for the plan at `plan_path` (Phase CJ). The high-water
/// mark is derived MARKER-FIRST:
///
///   - Marker present → it is authoritative and the `PLAN_ARCHIVE.md` scrape is
///     SKIPPED entirely (the whole point — an archived id survives in the
///     marker). We still take the max with live PLAN.md headers, so a phase a
///     hand-edit added above the marker self-corrects and can never collide.
///   - Marker absent (a pre-CJ plan) → fall back to scanning live PLAN.md +
///     `PLAN_ARCHIVE.md`, exactly as before, so an archived-out id is never
///     reused. The next write persists a marker (see [`seed_high_water_for_plan`]
///     / the archive + allocation paths), and the archive read stops happening.
pub fn next_phase_id_for_plan(plan_path: &Path) -> String {
    let plan_text = std::fs::read_to_string(plan_path).unwrap_or_default();
    let hw = match marker_of_text(&plan_text) {
        Some(marker) => {
            // Marker wins, but a live header above it (hand-edit) wins over the
            // marker — max of the two. No archive read.
            let live = high_water_of_texts(&plan_text, "");
            Some(max_seq(marker, live))
        }
        None => {
            let archive_text =
                std::fs::read_to_string(archive_path_for(plan_path)).unwrap_or_default();
            high_water_of_texts(&plan_text, &archive_text)
        }
    };
    next_after(hw)
}

/// The greater of `a` and an optional `b` by sequence order, defaulting to `a`
/// when `b` is `None`.
fn max_seq(a: String, b: Option<String>) -> String {
    match b {
        Some(b) if cmp_phase_ids(&b, &a) == Ordering::Greater => b,
        _ => a,
    }
}

/// Compute the high-water mark to persist as this plan's marker: the max over
/// live PLAN.md headers, the existing marker (if any), AND `PLAN_ARCHIVE.md`.
/// This is the ONE place the archive is consulted after migration — used when
/// first seeding the marker so it captures ids that were already swept. Returns
/// `None` for a plan that has never had a phase (nothing to pin yet).
pub fn seed_high_water_for_plan(plan_path: &Path) -> Option<String> {
    let plan_text = std::fs::read_to_string(plan_path).unwrap_or_default();
    let archive_text = std::fs::read_to_string(archive_path_for(plan_path)).unwrap_or_default();
    let from_headers = high_water_of_texts(&plan_text, &archive_text);
    let from_marker = marker_of_text(&plan_text);
    match (from_headers, from_marker) {
        (Some(h), Some(m)) => Some(max_seq(h, Some(m))),
        (Some(h), None) => Some(h),
        (None, Some(m)) => Some(m),
        (None, None) => None,
    }
}

/// Return `text` with the phase high-water marker set to `id` at the top: any
/// existing marker line(s) are dropped and a fresh one is prepended. Used by
/// `baseline` to seed the marker into an existing plan WITHOUT a full
/// parse/serialize round-trip, so the rest of the document is untouched.
pub fn set_marker_in_text(text: &str, id: &str) -> String {
    let mut out = render_high_water_marker(id);
    out.push('\n');
    for line in text.lines() {
        if parse_high_water_marker(line).is_some() {
            continue; // drop any stale marker line
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The greater of two optional sequence ids, or `None` when both are `None`.
fn max_opt(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), b) => Some(max_seq(a, b)),
        (None, b) => b,
    }
}

/// Advance the in-memory plan's high-water marker so it covers every phase
/// currently in `plan.phases` — and, on the FIRST seed (marker absent), the ids
/// already swept to `PLAN_ARCHIVE.md`. Monotonic: it never lowers the marker.
///
/// Call this while `plan.phases` still contains anything about to be archived
/// (i.e. BEFORE removing swept phases), so the swept id is captured in the
/// marker and can never be re-handed-out once its header leaves PLAN.md. That
/// archive path is the only place the marker is strictly load-bearing; `init`
/// seeds it for new plans and `baseline` seeds it for pre-CJ ones.
pub fn refresh_plan_marker(plan: &mut crate::ast::Plan, plan_path: &Path) {
    let live_hw: Option<String> = plan
        .phases
        .iter()
        .map(|p| p.id.as_str())
        .filter(|id| is_sequence_phase_id(id))
        .max_by(|a, b| cmp_phase_ids(a, b))
        .map(str::to_string);
    let new_marker = match plan.phase_high_water.clone() {
        // Marker already present → self-sustaining; fold in live headers only,
        // no archive read.
        Some(m) => Some(max_seq(m, live_hw)),
        // First seed: capture already-swept ids from the archive ONCE.
        None => {
            let archive_text =
                std::fs::read_to_string(archive_path_for(plan_path)).unwrap_or_default();
            max_opt(live_hw, high_water_of_texts("", &archive_text))
        }
    };
    if new_marker.is_some() {
        plan.phase_high_water = new_marker;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        crate::test_utils::scratch_dir("phase_seq")
    }

    fn write_plan_and_archive(plan: &str, archive: Option<&str>) -> PathBuf {
        let dir = scratch();
        let plan_path = dir.join("PLAN.md");
        std::fs::write(&plan_path, plan).unwrap();
        if let Some(a) = archive {
            std::fs::write(dir.join("PLAN_ARCHIVE.md"), a).unwrap();
        }
        plan_path
    }

    #[test]
    fn for_plan_marker_present_skips_archive() {
        // The core CJ contract: with a marker, the archive is NOT consulted.
        // Prove it by planting a bogus-HIGH archived id (`ZZ`) that would
        // dominate if scraped — next id must still derive from the marker
        // (`CI` -> `CJ`), NOT from `ZZ` (-> `AAA`).
        let plan_path = write_plan_and_archive(
            "<!-- plan-bridge:phase-high-water=CI -->\n# PLAN\n## Phase B - live\n",
            Some("## Phase ZZ - bogus, must be ignored\n"),
        );
        assert_eq!(next_phase_id_for_plan(&plan_path), "CJ");
    }

    #[test]
    fn for_plan_markerless_consults_archive() {
        // Migration path: no marker, a live phase LOWER than the archive
        // high-water. The archive MUST be read so the swept `CI` isn't reused —
        // next is `CJ`, not `C` (successor of the live `B`).
        let plan_path = write_plan_and_archive(
            "# PLAN\n## Phase B - live\n",
            Some("## Phase CA - swept\n## Phase CI - swept\n"),
        );
        assert_eq!(next_phase_id_for_plan(&plan_path), "CJ");
    }

    #[test]
    fn for_plan_marker_self_corrects_to_live_header_above_it() {
        // Marker is stale-low (`B`) but a hand-added live phase (`CI`) sits
        // above it. max(marker, live) wins -> next is `CJ`, no collision.
        let plan_path = write_plan_and_archive(
            "<!-- plan-bridge:phase-high-water=B -->\n# PLAN\n## Phase CI - hand added\n",
            None,
        );
        assert_eq!(next_phase_id_for_plan(&plan_path), "CJ");
    }

    #[test]
    fn for_plan_marker_ignores_garbled_live_header() {
        // Even with a marker, a garbled over-cap live header must not poison the
        // result. marker `CI` + garbled `CICJ` (ignored) -> `CJ`, not `CICK`.
        let plan_path = write_plan_and_archive(
            "<!-- plan-bridge:phase-high-water=CI -->\n# PLAN\n## Phase CICJ - garbled\n",
            None,
        );
        assert_eq!(next_phase_id_for_plan(&plan_path), "CJ");
    }

    #[test]
    fn for_plan_empty_is_a() {
        let plan_path = write_plan_and_archive("# PLAN\n", None);
        assert_eq!(next_phase_id_for_plan(&plan_path), "A");
    }

    #[test]
    fn seed_high_water_takes_max_over_live_marker_archive() {
        // Seeding the marker (migration) must capture the archive high-water so
        // a later marker-only read never re-hands-out a swept id.
        let plan_path = write_plan_and_archive(
            "<!-- plan-bridge:phase-high-water=B -->\n# PLAN\n## Phase D - live\n",
            Some("## Phase CI - swept\n"),
        );
        assert_eq!(seed_high_water_for_plan(&plan_path).as_deref(), Some("CI"));
    }

    #[test]
    fn seed_high_water_none_for_pristine_plan() {
        let plan_path = write_plan_and_archive("# PLAN\nno phases here\n", None);
        assert_eq!(seed_high_water_for_plan(&plan_path), None);
    }

    #[test]
    fn next_known_boundaries() {
        let cases = [
            ("A", "B"),
            ("B", "C"),
            ("Y", "Z"),
            ("Z", "AA"),
            ("AA", "AB"),
            ("AZ", "BA"),
            ("BY", "BZ"),
            ("BZ", "CA"),
            ("ZZ", "AAA"),
            ("AAZ", "ABA"),
        ];
        for (cur, want) in cases {
            assert_eq!(next_phase_id(cur).as_deref(), Some(want), "next({cur})");
        }
    }

    #[test]
    fn next_caps_at_max_len() {
        // Phase CJ: the namespace is bounded at MAX_PHASE_ID_LEN. The last id
        // (`Z`×cap) has no successor — incrementing it would grow past the cap,
        // so `next_phase_id` returns None rather than mint an over-cap id. That
        // is genuine exhaustion, and the caller must treat it as an error.
        let last = "Z".repeat(MAX_PHASE_ID_LEN);
        assert_eq!(
            next_phase_id(&last),
            None,
            "exhausted namespace has no next"
        );
        // An over-cap token is not a sequence member: no successor either.
        let over_cap = "A".repeat(MAX_PHASE_ID_LEN + 1);
        assert_eq!(
            next_phase_id(&over_cap),
            None,
            "over-cap id is not in sequence"
        );
        // The id just under the ceiling still increments normally.
        assert_eq!(next_phase_id("ZZY").as_deref(), Some("ZZZ"));
    }

    #[test]
    fn next_rejects_non_alpha() {
        for bad in ["", "a", "Az", "A1", "1", "42", "A.B", "A ", " A", "Ä"] {
            assert_eq!(next_phase_id(bad), None, "expected None for {bad:?}");
        }
    }

    #[test]
    fn next_is_strictly_increasing() {
        // Walk the first few thousand ids; each must compare greater than the
        // previous and round through the length boundaries in order.
        let mut id = "A".to_string();
        for _ in 0..5000 {
            let nxt = next_phase_id(&id).unwrap();
            assert_eq!(cmp_phase_ids(&nxt, &id), Ordering::Greater, "{nxt} vs {id}");
            id = nxt;
        }
    }

    #[test]
    fn cmp_orders_by_length_then_lex() {
        assert_eq!(cmp_phase_ids("Z", "AA"), Ordering::Less);
        assert_eq!(cmp_phase_ids("AA", "Z"), Ordering::Greater);
        assert_eq!(cmp_phase_ids("AY", "AZ"), Ordering::Less);
        assert_eq!(cmp_phase_ids("AZ", "BA"), Ordering::Less);
        assert_eq!(cmp_phase_ids("BY", "BZ"), Ordering::Less);
        assert_eq!(cmp_phase_ids("ZZ", "AAA"), Ordering::Less);
        assert_eq!(cmp_phase_ids("BY", "BY"), Ordering::Equal);
    }

    #[test]
    fn is_alpha_phase_id_classifies() {
        for ok in ["A", "Z", "AA", "BZ", "ZZ", "AAA"] {
            assert!(is_alpha_phase_id(ok), "{ok:?} should be valid");
        }
        for bad in ["", "a", "A1", "1", "42", "A.B", "A ", "Ä"] {
            assert!(!is_alpha_phase_id(bad), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn scan_extracts_sequence_phase_ids_only() {
        let text = "\
# PLAN
## Phase BY - Older alpha
- [ ] BY.1 - a task, not a phase
## Phase 42 - Legacy numeric (ignored)
### Phase AA - deeper heading still counts
## Phase CICJ - garbled/concatenated header (over cap, ignored)
## Notes
## Phase CA *(depends on: BY)*
prose mentioning Phase ZZ inline should be ignored
";
        let got: Vec<&str> = scan_sequence_phase_ids(text).collect();
        // `42` (numeric) and `CICJ` (over cap) are both excluded — the latter
        // is exactly the garbled header that would otherwise poison next-id.
        assert_eq!(got, vec!["BY", "AA", "CA"]);
    }

    #[test]
    fn is_sequence_phase_id_enforces_cap() {
        for ok in ["A", "Z", "AA", "ZZ", "AAA", "ZZZ"] {
            assert!(is_sequence_phase_id(ok), "{ok:?} should be in sequence");
            // Shape check is looser — it accepts these too, cap or not.
            assert!(is_alpha_phase_id(ok));
        }
        for over in ["AAAA", "CICJ", "ZZZZ"] {
            assert!(!is_sequence_phase_id(over), "{over:?} is over the cap");
            // ...but they are still syntactically alpha (not numeric).
            assert!(is_alpha_phase_id(over), "{over:?} is still alpha-shaped");
        }
    }

    #[test]
    fn next_from_texts_ignores_garbled_over_cap_header() {
        // The reported bug: a concatenated `## Phase CICJ` alongside real
        // phases must NOT become the high-water. Next id derives from `CA`
        // (the real max), giving `CB` — not `CICK` (poisoned) or a reset `A`.
        let plan = "## Phase BY - real\n## Phase CICJ - garbled\n## Phase CA - real\n";
        assert_eq!(next_phase_id_from_texts(plan, ""), "CB");
    }

    #[test]
    fn high_water_marker_render_parse_roundtrip() {
        for id in ["A", "CI", "ZZZ"] {
            let line = render_high_water_marker(id);
            assert_eq!(line, format!("<!-- plan-bridge:phase-high-water={id} -->"));
            assert_eq!(parse_high_water_marker(&line), Some(id));
        }
        // Whitespace tolerance and indentation inside/around the comment.
        assert_eq!(
            parse_high_water_marker("  <!--  plan-bridge:phase-high-water = CI  -->  "),
            Some("CI")
        );
        // Non-marker lines yield None.
        for miss in [
            "# PLAN",
            "<!-- some other comment -->",
            "## Phase CI - Work",
            "plan-bridge:phase-high-water=CI", // not a comment
        ] {
            assert_eq!(parse_high_water_marker(miss), None, "{miss:?}");
        }
    }

    #[test]
    fn next_from_texts_exhaustion_returns_high_water_not_a() {
        // When the only sequence id present is the last in the namespace
        // (`Z`×cap), there is no successor. Rather than silently reset to `A`
        // (a guaranteed collision), return the high-water itself so the
        // caller's existing "phase already exists" guard fires.
        let last = "Z".repeat(MAX_PHASE_ID_LEN);
        let plan = format!("## Phase {last} - final\n");
        assert_eq!(next_phase_id_from_texts(&plan, ""), last);
    }

    #[test]
    fn next_from_texts_empty_is_a() {
        assert_eq!(next_phase_id_from_texts("", ""), "A");
        assert_eq!(
            next_phase_id_from_texts("# PLAN\n## Phase 1 - nothing\n", ""),
            "A"
        );
    }

    #[test]
    fn next_from_texts_ignores_legacy_numeric() {
        let plan = "## Phase 42 - legacy\n## Phase BY - alpha\n";
        assert_eq!(next_phase_id_from_texts(plan, ""), "BZ");
    }

    #[test]
    fn next_from_texts_includes_archive_high_water_mark() {
        // The real scenario this phase was born in: BZ is live, CA was already
        // swept to the archive. The next id must clear BOTH — CB, not CA.
        let plan = "## Phase BZ - live work\n";
        let archive = "## Phase BY - swept\n## Phase CA - swept\n";
        assert_eq!(next_phase_id_from_texts(plan, archive), "CB");
    }

    #[test]
    fn next_from_texts_rolls_length_boundary() {
        assert_eq!(
            next_phase_id_from_texts("## Phase AA - x\n", "## Phase ZZ - y\n"),
            "AAA"
        );
    }
}

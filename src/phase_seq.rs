//! Phase BZ: the phase-name sequence. Phase ids are uppercase-letter tokens
//! incrementing in bijective base-26 (spreadsheet-column order):
//! `A..Z` -> `AA..AZ` -> `BA..BZ` -> ... -> `ZZ` -> `AAA` -> ...
//!
//! This is *bijective* base-26 (no zero digit): `A` is the first id, `Z` rolls
//! over to `AA` (not to a leading-zero two-letter form). The operations work
//! directly on the string — an odometer-style increment and a (length, then
//! lexicographic) comparison — so they support **arbitrarily long** ids with
//! no fixed-width-integer ceiling. This module is the single source of truth
//! for the ordering; [`next_phase_id`] (BZ.2) and the high-water-mark scan
//! (BZ.3) build on it.
//!
//! Uppercase A-Z only, by deliberate project policy: numeric phase ids (`1`,
//! `42`) are legacy and are not part of this sequence — they fail
//! [`is_alpha_phase_id`] and are ignored when reconstructing the latest id.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// True when `s` is a well-formed member of the phase-name sequence: a
/// non-empty run of uppercase ASCII letters and nothing else. Rejects the
/// empty string, lowercase, digits, dots, spaces, and non-ASCII.
pub fn is_alpha_phase_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_uppercase())
}

/// The successor of an uppercase-letter phase id in the sequence:
/// `A -> B`, `Z -> AA`, `AZ -> BA`, `ZZ -> AAA`, `BY -> BZ`. Implemented as an
/// odometer with carry — a run of trailing `Z`s rolls to `A`s and grows the id
/// by one digit when the carry runs off the front — so it works for ids of any
/// length, well past what any fixed-width integer could hold. Returns `None`
/// when `current` is not a valid alpha phase id (e.g. a legacy numeric id);
/// such ids are not part of the sequence and have no defined successor here.
pub fn next_phase_id(current: &str) -> Option<String> {
    if !is_alpha_phase_id(current) {
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

/// Extract the alpha phase ids from one markdown document by matching FORMATv2
/// phase headers (`## Phase <id> - <title>`, at any heading level). The id is
/// the first whitespace-delimited token after `Phase`; only ids that pass
/// [`is_alpha_phase_id`] are yielded, so legacy numeric (`## Phase 42`) and
/// dotted ids are skipped — they are not part of the sequence.
fn scan_alpha_phase_ids(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter_map(|line| {
        let after_hashes = line.trim_start().trim_start_matches('#');
        // Require at least one `#` was stripped AND a space separated it.
        if after_hashes.len() == line.trim_start().len() {
            return None;
        }
        let id = after_hashes.trim_start().strip_prefix("Phase ")?;
        let id = id.split_whitespace().next()?;
        is_alpha_phase_id(id).then_some(id)
    })
}

/// Reconstruct the next phase id by scanning the live plan text and the archive
/// text for existing alpha phase ids, taking the highest, and returning its
/// successor. Returns `"A"` when neither document has any alpha phase id (a
/// brand-new or all-legacy-numeric project). Scanning the archive too is what
/// stops the sequence from ever re-handing-out an id that was already swept.
pub fn next_phase_id_from_texts(plan_text: &str, archive_text: &str) -> String {
    let mut best: Option<&str> = None;
    for text in [plan_text, archive_text] {
        for id in scan_alpha_phase_ids(text) {
            if best.is_none_or(|b| cmp_phase_ids(id, b) == Ordering::Greater) {
                best = Some(id);
            }
        }
    }
    best.and_then(next_phase_id)
        .unwrap_or_else(|| "A".to_string())
}

/// Sibling `PLAN_ARCHIVE.md` next to a `PLAN.md` path.
pub fn archive_path_for(plan_path: &Path) -> PathBuf {
    plan_path.with_file_name("PLAN_ARCHIVE.md")
}

/// File-reading wrapper over [`next_phase_id_from_texts`]: reads `plan_path`
/// and its sibling `PLAN_ARCHIVE.md` (each treated as empty if absent) and
/// reconstructs the next phase id.
pub fn next_phase_id_for_plan(plan_path: &Path) -> String {
    let plan_text = std::fs::read_to_string(plan_path).unwrap_or_default();
    let archive_text = std::fs::read_to_string(archive_path_for(plan_path)).unwrap_or_default();
    next_phase_id_from_texts(&plan_text, &archive_text)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            ("ZZZ", "AAAA"),
        ];
        for (cur, want) in cases {
            assert_eq!(next_phase_id(cur).as_deref(), Some(want), "next({cur})");
        }
    }

    #[test]
    fn next_supports_arbitrary_length() {
        // Well beyond any u64-ordinal ceiling (~13 chars): a 40-digit id still
        // increments, and a 40-`Z` id grows to 41 `A`s.
        let forty_z = "Z".repeat(40);
        assert_eq!(next_phase_id(&forty_z), Some("A".repeat(41)));
        let long = format!("{}A", "M".repeat(50)); // ...MA -> ...MB
        assert_eq!(next_phase_id(&long), Some(format!("{}B", "M".repeat(50))));
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
    fn scan_extracts_alpha_phase_ids_only() {
        let text = "\
# PLAN
## Phase BY - Older alpha
- [ ] BY.1 - a task, not a phase
## Phase 42 - Legacy numeric (ignored)
### Phase AA - deeper heading still counts
## Notes
## Phase CA *(depends on: BY)*
prose mentioning Phase ZZ inline should be ignored
";
        let got: Vec<&str> = scan_alpha_phase_ids(text).collect();
        assert_eq!(got, vec!["BY", "AA", "CA"]);
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

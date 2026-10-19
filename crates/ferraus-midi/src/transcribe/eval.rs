//! Note-level transcription metrics in the style of `mir_eval.transcription`.
//!
//! A reference and an estimated note may be matched when their onsets differ by at most
//! `onset_tolerance_s` and their pitches by at most `pitch_tolerance_cents`. Offsets are
//! ignored (mir_eval's `offset_ratio=None`). The matching is a maximum bipartite matching
//! (augmenting paths, like mir_eval's Hopcroft-Karp), so each note is used at most once;
//! precision, recall and F-measure follow from the match count.

use serde::Serialize;

/// A note for evaluation: onset (s), offset (s), pitch (fractional MIDI).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvalNote {
    pub onset_s: f64,
    pub offset_s: f64,
    pub midi: f64,
}

/// Precision, recall, F-measure and the matched index pairs.
#[derive(Clone, Debug, Serialize)]
pub struct NoteScores {
    pub precision: f64,
    pub recall: f64,
    pub f_measure: f64,
    pub matched: usize,
    pub reference: usize,
    pub estimated: usize,
    #[serde(skip)]
    pub pairs: Vec<(usize, usize)>,
}

/// Match notes and score them. `pitch_tolerance_cents` = 50 is mir_eval's default; use a
/// value below 50 (e.g. 1) with integer pitches for "exact pitch".
pub fn match_notes(
    reference: &[EvalNote],
    estimated: &[EvalNote],
    onset_tolerance_s: f64,
    pitch_tolerance_cents: f64,
) -> NoteScores {
    let adj: Vec<Vec<usize>> = reference
        .iter()
        .map(|r| {
            estimated
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    (r.onset_s - e.onset_s).abs() <= onset_tolerance_s + 1e-9
                        && (r.midi - e.midi).abs() * 100.0 <= pitch_tolerance_cents + 1e-9
                })
                .map(|(j, _)| j)
                .collect()
        })
        .collect();
    let mut owner: Vec<Option<usize>> = vec![None; estimated.len()];
    fn augment(i: usize, adj: &[Vec<usize>], seen: &mut [bool], owner: &mut [Option<usize>]) -> bool {
        for &j in &adj[i] {
            if seen[j] {
                continue;
            }
            seen[j] = true;
            if owner[j].is_none() || augment(owner[j].unwrap(), adj, seen, owner) {
                owner[j] = Some(i);
                return true;
            }
        }
        false
    }
    for i in 0..reference.len() {
        let mut seen = vec![false; estimated.len()];
        augment(i, &adj, &mut seen, &mut owner);
    }
    let mut pairs: Vec<(usize, usize)> = owner.iter().enumerate().filter_map(|(j, o)| o.map(|i| (i, j))).collect();
    pairs.sort();
    let m = pairs.len();
    let precision = if estimated.is_empty() { 0.0 } else { m as f64 / estimated.len() as f64 };
    let recall = if reference.is_empty() { 0.0 } else { m as f64 / reference.len() as f64 };
    let f = if precision + recall > 0.0 { 2.0 * precision * recall / (precision + recall) } else { 0.0 };
    NoteScores {
        precision,
        recall,
        f_measure: f,
        matched: m,
        reference: reference.len(),
        estimated: estimated.len(),
        pairs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(on: f64, m: f64) -> EvalNote {
        EvalNote { onset_s: on, offset_s: on + 0.2, midi: m }
    }

    #[test]
    fn perfect_and_partial() {
        let r = [n(0.0, 60.0), n(0.5, 62.0), n(1.0, 64.0)];
        let s = match_notes(&r, &r, 0.05, 1.0);
        assert_eq!(s.matched, 3);
        assert!((s.f_measure - 1.0).abs() < 1e-12);
        // one wrong pitch, one late onset, one extra note
        let e = [n(0.01, 60.0), n(0.5, 63.0), n(1.07, 64.0), n(1.5, 65.0)];
        let s = match_notes(&r, &e, 0.05, 1.0);
        assert_eq!(s.matched, 1);
        assert!((s.precision - 0.25).abs() < 1e-12);
        assert!((s.recall - 1.0 / 3.0).abs() < 1e-12);
        assert!(match_notes(&[], &e, 0.05, 1.0).f_measure == 0.0);
    }

    #[test]
    fn maximum_matching_not_greedy() {
        // greedy would give est 0 to ref 0 and leave ref 1 unmatched
        let r = [n(0.00, 60.0), n(0.06, 60.0)];
        let e = [n(0.03, 60.0), n(-0.03, 60.0)];
        let s = match_notes(&r, &e, 0.05, 1.0);
        assert_eq!(s.matched, 2);
    }
}

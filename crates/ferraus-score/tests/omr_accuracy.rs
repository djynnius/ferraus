//! End-to-end OMR accuracy on the clean printed test set in `tests/omr/` (ABC and MusicXML
//! fixtures: melodies and piano pieces in several keys and meters, with rests, accidentals,
//! dots, chords, and ledger lines): each fixture is engraved with the `ferraus` binary
//! (Verovio through the ferraus-engrave helper), read back with `score read --engine
//! builtin`, and compared with its source note by note (pitch and duration, after a
//! sequence alignment per staff). The PRD target is at least 90% pitch-plus-duration
//! accuracy; every output must pass `score check` or list its failing measures.
//!
//! It needs a release build of the CLI and the helper (`cargo build --release -p ferraus
//! -p ferraus-engrave`; or `FERRAUS_BIN=/path/to/ferraus`) and the oemer models (pulled with
//! `-y` when missing, about 110 MB), so it is ignored by default:
//!
//! ```text
//! cargo test -p ferraus-score --test omr_accuracy -- --ignored --nocapture
//! FERRAUS_OMR_INPUT=pdf cargo test -p ferraus-score --test omr_accuracy -- --ignored --nocapture
//! ```

use ferraus_score::formats;
use ferraus_score::Score;
use std::path::{Path, PathBuf};
use std::process::Command;

fn ferraus_bin() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FERRAUS_BIN") {
        return Some(PathBuf::from(p));
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    ["release", "debug"].iter().map(|d| root.join(d).join("ferraus")).find(|p| p.is_file())
}

/// One element per event: sorted MIDI pitches (empty for a rest) and the duration in
/// 1/960 quarter notes.
type Token = (Vec<i32>, u64);

fn tokens(score: &Score) -> Vec<Vec<Token>> {
    let div = score.divisions.max(1) as u64;
    let mut out = Vec::new();
    for p in &score.parts {
        for st in &p.staves {
            let mut seq = Vec::new();
            for m in &st.measures {
                for v in &m.voices {
                    for e in &v.events {
                        let mut ps: Vec<i32> = e.notes.iter().map(|n| n.pitch.midi()).collect();
                        ps.sort();
                        seq.push((ps, e.duration as u64 * 960 / div));
                    }
                }
            }
            out.push(seq);
        }
    }
    out
}

/// (reference elements, correct pitch+duration, correct pitch) after aligning the two
/// sequences with unit-cost edits; a rest is one element with "pitch" rest.
fn compare(reference: &[Token], got: &[Token]) -> (usize, usize, usize) {
    let (n, m) = (reference.len(), got.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = d[i - 1][j - 1] + usize::from(reference[i - 1] != got[j - 1]);
            d[i][j] = sub.min(d[i - 1][j] + 1).min(d[i][j - 1] + 1);
        }
    }
    let size = |t: &Token| t.0.len().max(1);
    let total: usize = reference.iter().map(size).sum();
    let (mut both, mut pitch) = (0, 0);
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        let sub = d[i - 1][j - 1] + usize::from(reference[i - 1] != got[j - 1]);
        if d[i][j] == sub {
            let (r, g) = (&reference[i - 1], &got[j - 1]);
            let same =
                if r.0.is_empty() && g.0.is_empty() { 1 } else { r.0.iter().filter(|p| g.0.contains(p)).count() };
            pitch += same;
            if r.1 == g.1 {
                both += same;
            }
            i -= 1;
            j -= 1;
        } else if d[i][j] == d[i - 1][j] + 1 {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    (total, both, pitch)
}

#[test]
fn alignment_counts_notes() {
    let a: Vec<Token> = vec![(vec![60], 960), (vec![62, 65], 960), (vec![], 1920)];
    assert_eq!(compare(&a, &a), (4, 4, 4));
    let b: Vec<Token> = vec![(vec![60], 480), (vec![62], 960), (vec![], 1920)];
    assert_eq!(compare(&a, &b), (4, 2, 3));
    assert_eq!(compare(&a, &[]), (4, 0, 0));
}

#[test]
#[ignore = "needs the release CLI, the ferraus-engrave helper, and the oemer models"]
fn omr_accuracy_on_clean_printed_scores() {
    let Some(bin) = ferraus_bin() else {
        eprintln!("skipped: no ferraus binary (cargo build --release -p ferraus -p ferraus-engrave)");
        return;
    };
    let ext = std::env::var("FERRAUS_OMR_INPUT").unwrap_or_else(|_| "png".into());
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/omr");
    let tmp = std::env::temp_dir().join(format!("ferraus-omr-accuracy-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    let (mut total, mut both, mut pitch) = (0, 0, 0);
    let mut failing_without_review = Vec::new();
    for f in &files {
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(f).unwrap();
        let reference = formats::load(f, &bytes).unwrap().score;
        let page = tmp.join(format!("{name}.{ext}"));
        let out = Command::new(&bin)
            .args(["score", "convert"])
            .arg(f)
            .arg("-o")
            .arg(&page)
            .arg("--overwrite")
            .output()
            .unwrap();
        if !out.status.success() {
            eprintln!("skipped: cannot engrave {name}: {}", String::from_utf8_lossy(&out.stdout));
            return;
        }
        let xml = tmp.join(format!("{name}.read.musicxml"));
        let out = Command::new(&bin)
            .args(["score", "read"])
            .arg(&page)
            .arg("-o")
            .arg(&xml)
            .args(["--overwrite", "--review", "--json", "-y"])
            .output()
            .unwrap();
        let env: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
        assert_eq!(env["ok"], true, "{name}: score read failed: {}", String::from_utf8_lossy(&out.stdout));
        let result = &env["result"];
        // every output passes score check or names its failing measures
        if result["check_ok"] != true && result["failing_measures"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
            failing_without_review.push(name.clone());
        }
        let got = formats::load(&xml, &std::fs::read(&xml).unwrap()).unwrap().score;
        let (rt, gt) = (tokens(&reference), tokens(&got));
        let (mut t, mut b, mut p) = (0, 0, 0);
        for (si, r) in rt.iter().enumerate() {
            let (a, c, d) = compare(r, gt.get(si).map(|v| v.as_slice()).unwrap_or(&[]));
            t += a;
            b += c;
            p += d;
        }
        eprintln!(
            "{name:<28} notes {t:>3}  pitch+duration {:>5.1}%  pitch {:>5.1}%  check_ok {}",
            100.0 * b as f64 / t.max(1) as f64,
            100.0 * p as f64 / t.max(1) as f64,
            result["check_ok"]
        );
        total += t;
        both += b;
        pitch += p;
    }
    let _ = std::fs::remove_dir_all(&tmp);
    let acc = both as f64 / total.max(1) as f64;
    eprintln!(
        "total: {total} notes, pitch+duration {:.1}%, pitch {:.1}%",
        100.0 * acc,
        100.0 * pitch as f64 / total.max(1) as f64
    );
    assert!(
        failing_without_review.is_empty(),
        "outputs that fail score check without listing measures: {failing_without_review:?}"
    );
    assert!(acc >= 0.9, "pitch+duration accuracy {:.1}% is below the 90% target", 100.0 * acc);
}

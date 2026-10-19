//! Acceptance: notation → MIDI → notation reproduces pitches and durations exactly; the
//! writers' output round-trips through the readers; `score check` passes on our output.
#![allow(clippy::field_reassign_with_default, clippy::manual_is_multiple_of, clippy::needless_range_loop)]

mod common;
use common::*;
use ferraus_score::check::check;
use ferraus_score::midi::{read_midi, write_midi};
use ferraus_score::model::*;
use ferraus_score::notate::{notate, NotateOptions};
use ferraus_score::perform::{perform, PlayOptions};
use ferraus_score::{abc, lily, mei, musicxml};

fn fixtures() -> Vec<(&'static str, Score)> {
    let c = KeySig::major(0);
    vec![
        (
            "melody",
            score(
                c,
                TimeSig::new(4, 4),
                false,
                &[
                    &["E4:q", "E4:q", "F4:q", "G4:q"],
                    &["G4:q", "F4:q", "E4:q", "D4:q"],
                    &["C4:q", "C4:q", "D4:q", "E4:q"],
                    &["E4:q.", "D4:e", "D4:h"],
                ],
            ),
        ),
        (
            "chords",
            score(c, TimeSig::new(4, 4), false, &[&["C4+E4+G4:h", "F4+A4+C5:h"], &["G3+B3+D4+F4:h", "C4+E4+G4:h"]]),
        ),
        (
            "triplets",
            score(
                c,
                TimeSig::new(4, 4),
                false,
                &[&["C5:q", "D5:e3", "E5:e3", "F5:e3", "G5:q", "A5:e3", "G5:e3", "F5:e3"], &["E5:h", "r:q", "C5:q"]],
            ),
        ),
        (
            "ties-across-bars",
            score(c, TimeSig::new(4, 4), false, &[&["C4:q", "D4:q", "E4:h~"], &["E4:q", "F4:q", "G4:h"]]),
        ),
        (
            "pickup-3-4",
            score(
                KeySig::major(-1),
                TimeSig::new(3, 4),
                true,
                &[&["C4:q"], &["F4:q", "F4:q", "G4:q"], &["A4:h", "Bb4:q"], &["C5:h."]],
            ),
        ),
        (
            "six-eight",
            score(
                KeySig::major(2),
                TimeSig::new(6, 8),
                false,
                &[
                    &["D4:q.", "F#4:e", "G4:e", "A4:e"],
                    &["B4:q", "A4:e", "F#4:q."],
                    &["E4:e", "F#4:e", "G4:e", "A4:q."],
                ],
            ),
        ),
        (
            "sixteenths-dotted",
            score(
                KeySig::major(1),
                TimeSig::new(2, 4),
                false,
                &[&["G4:s", "A4:s", "B4:s", "C5:s", "D5:e.", "B4:s"], &["C5:e", "r:e", "A4:q"]],
            ),
        ),
        (
            "rests",
            score(
                KeySig::minor(1),
                TimeSig::new(4, 4),
                false,
                &[&["r:q", "E4:q", "r:e", "F#4:e", "G4:q"], &["D#4:h", "E4:h"]],
            ),
        ),
    ]
}

fn render_and_notate(s: &Score, opts: &NotateOptions) -> Score {
    let (m, _) = perform(s, &PlayOptions::default());
    let bytes = write_midi(&m).unwrap();
    let back = read_midi(&bytes).unwrap();
    notate(&back, opts).0
}

#[test]
fn midi_round_trip_reproduces_pitches_and_durations() {
    for (name, s) in fixtures() {
        let out = render_and_notate(&s, &NotateOptions::default());
        assert_eq!(flat_notes(&out, 0, 0), flat_notes(&s, 0, 0), "{name}: notes differ");
        // same meter, key, and pickup
        let st = &out.parts[0].staves[0];
        let src = &s.parts[0].staves[0];
        assert_eq!(Score::time_at(st, 0), Score::time_at(src, 0), "{name}: time");
        assert_eq!(Score::key_at(st, 0), Score::key_at(src, 0), "{name}: key");
        assert_eq!(st.measures[0].implicit, src.measures[0].implicit, "{name}: pickup");
        assert_eq!(st.measures.len(), src.measures.len(), "{name}: measure count");
        // spelled pitches are identical too
        let names = |sc: &Score| -> Vec<String> {
            sc.parts[0].staves[0]
                .measures
                .iter()
                .flat_map(|m| {
                    m.voices.iter().flat_map(|v| v.events.iter().flat_map(|e| e.notes.iter().map(|n| n.pitch.name())))
                })
                .collect()
        };
        let mut a = names(&out);
        let mut b = names(&s);
        a.dedup();
        b.dedup();
        assert_eq!(a, b, "{name}: spelling");
        let r = check(&out, &[]);
        assert!(r.ok, "{name}: check failed: {:?}", r.issues);
    }
}

#[test]
fn triplets_are_detected_and_bracketed() {
    let (_, s) = fixtures().into_iter().find(|f| f.0 == "triplets").unwrap();
    let out = render_and_notate(&s, &NotateOptions::default());
    let evs = &out.parts[0].staves[0].measures[0].voices[0].events;
    let trip: Vec<&Event> = evs.iter().filter(|e| e.tuplet.is_some()).collect();
    assert_eq!(trip.len(), 6);
    assert!(trip[0].tuplet.unwrap().start && trip[2].tuplet.unwrap().stop);
    let xml = musicxml::write(&out);
    assert!(xml.contains("<actual-notes>3</actual-notes>"));
    assert!(xml.contains("<tuplet type=\"start\""));
}

#[test]
fn ties_cross_bar_lines() {
    let (_, s) = fixtures().into_iter().find(|f| f.0 == "ties-across-bars").unwrap();
    let out = render_and_notate(&s, &NotateOptions::default());
    let m1 = &out.parts[0].staves[0].measures[0].voices[0].events;
    let m2 = &out.parts[0].staves[0].measures[1].voices[0].events;
    assert!(m1.last().unwrap().notes[0].tie_start);
    assert!(m2[0].notes[0].tie_stop);
    assert!(musicxml::write(&out).contains("<tied type=\"start\"/>"));
}

#[test]
fn grand_staff_round_trip_with_split() {
    let rh = measures(&[&["E5:q", "D5:q", "C5:h"], &["G5:h.", "r:q"]]);
    let lh = measures(&[&["C3+G3:h", "E3+G3:h"], &["G2+D3:w"]]);
    let s = score_with(KeySig::major(0), TimeSig::new(4, 4), false, vec![(Clef::treble(), rh), (Clef::bass(), lh)]);
    let mut opts = NotateOptions::default();
    opts.layout = ferraus_score::notate::Layout::Grand;
    opts.split = ferraus_score::notate::Split::At(60);
    let out = render_and_notate(&s, &opts);
    assert_eq!(out.parts[0].staves.len(), 2);
    assert_eq!(flat_notes(&out, 0, 0), flat_notes(&s, 0, 0));
    assert_eq!(flat_notes(&out, 0, 1), flat_notes(&s, 0, 1));
    // auto split keeps the hands apart too
    opts.split = ferraus_score::notate::Split::Auto;
    let auto = render_and_notate(&s, &opts);
    assert_eq!(flat_notes(&auto, 0, 0), flat_notes(&s, 0, 0));
    assert_eq!(flat_notes(&auto, 0, 1), flat_notes(&s, 0, 1));
    let xml = musicxml::write(&auto);
    assert!(xml.contains("<staves>2</staves>") && xml.contains("<clef number=\"2\">"));
    assert!(check(&auto, &[]).ok);
}

#[test]
fn two_voices_on_one_staff() {
    // a held bass note under a moving melody
    let mut s = score(KeySig::major(0), TimeSig::new(4, 4), false, &[&["E5:q", "F5:q", "G5:q", "E5:q"]]);
    let low = measures(&[&["C4:w"]]);
    s.parts[0].staves[0].measures[0].voices.push(Voice { number: 2, events: low[0].clone() });
    let out = render_and_notate(&s, &NotateOptions::default());
    let m = &out.parts[0].staves[0].measures[0];
    assert_eq!(m.voices.len(), 2);
    assert_eq!(flat_notes(&out, 0, 0), flat_notes(&s, 0, 0));
    assert!(check(&out, &[]).ok);
}

#[test]
fn musicxml_write_read_write_is_stable_and_checks_pass() {
    let mut all: Vec<(String, Score)> = fixtures().into_iter().map(|(n, s)| (n.to_string(), s)).collect();
    // notated versions (with confidence, chords, velocities)
    for (n, s) in fixtures() {
        all.push((format!("{n}-notated"), render_and_notate(&s, &NotateOptions::default())));
    }
    for (name, s) in all {
        let x1 = musicxml::write(&s);
        let (s2, unsupported) = musicxml::read(&x1).unwrap();
        assert!(unsupported.is_empty(), "{name}: {unsupported:?}");
        let x2 = musicxml::write(&s2);
        assert_eq!(x1, x2, "{name}: MusicXML not stable");
        assert!(check(&s2, &[]).ok, "{name}: {:?}", check(&s2, &[]).issues);
        assert_eq!(flat_notes(&s2, 0, 0), flat_notes(&s, 0, 0));
        // compressed container too
        let mxl = musicxml::write_mxl(&s).unwrap();
        let (s3, _) = musicxml::read_bytes(&mxl).unwrap();
        assert_eq!(musicxml::write(&s3), x1);
    }
}

#[test]
fn abc_lilypond_mei_round_trip_notes() {
    for (name, s) in fixtures() {
        let a = abc::write(&s);
        let sa = abc::read(&a).unwrap_or_else(|e| panic!("{name}: abc read: {e}\n{a}"));
        assert_eq!(flat_notes(&sa, 0, 0), flat_notes(&s, 0, 0), "{name}: ABC\n{a}");
        assert!(check(&sa, &[]).ok, "{name}: ABC check {:?}", check(&sa, &[]).issues);

        let l = lily::write(&s);
        let sl = lily::read(&l).unwrap_or_else(|e| panic!("{name}: ly read: {e}\n{l}"));
        assert_eq!(flat_notes(&sl, 0, 0), flat_notes(&s, 0, 0), "{name}: LilyPond\n{l}");
        assert!(check(&sl, &[]).ok, "{name}: ly check {:?}", check(&sl, &[]).issues);

        let m = mei::write(&s);
        let sm = mei::read(&m).unwrap_or_else(|e| panic!("{name}: mei read: {e}"));
        assert_eq!(flat_notes(&sm, 0, 0), flat_notes(&s, 0, 0), "{name}: MEI");
        assert!(!musicxml::write(&sm).is_empty());
        assert!(check(&sm, &[]).ok, "{name}: MEI check {:?}", check(&sm, &[]).issues);
    }
}

#[test]
fn json_model_round_trip() {
    for (name, s) in fixtures() {
        let j = serde_json::to_string_pretty(&s).unwrap();
        let back: Score = serde_json::from_str(&j).unwrap();
        assert_eq!(back, s, "{name}");
    }
}

/// Writes every fixture's MusicXML (plus notated, drum, chord-diagram, and lyric
/// variants) to `$FERRAUS_SCORE_DUMP` for external validation, e.g.
/// `xmllint --noout --schema musicxml.xsd *.musicxml`. A no-op otherwise.
#[test]
fn dump_outputs_for_schema_validation() {
    let Some(dir) = std::env::var_os("FERRAUS_SCORE_DUMP") else { return };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, s) in fixtures() {
        std::fs::write(dir.join(format!("{name}.musicxml")), musicxml::write(&s)).unwrap();
        let mut opts = NotateOptions::default();
        opts.chords = ferraus_score::notate::ChordMode::Diagrams;
        opts.lyrics = Some("la la-la la la la la la la la la la la".into());
        let mut n = render_and_notate(&s, &opts);
        ferraus_score::solfa::annotate(&mut n, ferraus_score::solfa::SolfaKind::Movable);
        std::fs::write(dir.join(format!("{name}-notated.musicxml")), musicxml::write(&n)).unwrap();
        std::fs::write(dir.join(format!("{name}.mei")), mei::write(&s)).unwrap();
        std::fs::write(dir.join(format!("{name}.ly")), lily::write(&n)).unwrap();
        std::fs::write(dir.join(format!("{name}.abc")), abc::write(&n)).unwrap();
        std::fs::write(dir.join(format!("{name}.svg")), ferraus_score::svg::render(&n)).unwrap();
    }
    // a drum score and a grand staff
    let mut d = ferraus_score::midi::MidiData {
        ppq: 480,
        tempos: vec![(0, 500_000)],
        time_sigs: vec![(0, 4, 4)],
        ..Default::default()
    };
    for k in 0..16u64 {
        d.notes.push(ferraus_score::midi::MidiNote {
            track: 0,
            channel: 9,
            key: 42,
            velocity: 80,
            start: k * 240,
            end: k * 240 + 100,
        });
    }
    for (t, key) in [
        (0u64, 36u8),
        (480, 38),
        (960, 36),
        (1440, 38),
        (1920, 49),
        (1920, 36),
        (2400, 38),
        (2880, 36),
        (3120, 36),
        (3360, 38),
    ] {
        d.notes.push(ferraus_score::midi::MidiNote {
            track: 0,
            channel: 9,
            key,
            velocity: 100,
            start: t,
            end: t + 100,
        });
    }
    d.notes.sort_by_key(|n| n.start);
    let (drums, _) = notate(&d, &NotateOptions::default());
    std::fs::write(dir.join("drums.musicxml"), musicxml::write(&drums)).unwrap();
    std::fs::write(dir.join("drums.svg"), ferraus_score::svg::render(&drums)).unwrap();
    let rh = measures(&[&["E5:e", "D5:e", "C5:q", "G5:h"], &["F5:q.", "E5:e", "D5:h"]]);
    let lh = measures(&[&["C3+G3:h", "E3+G3:h"], &["G2+D3:w"]]);
    let g = score_with(KeySig::major(0), TimeSig::new(4, 4), false, vec![(Clef::treble(), rh), (Clef::bass(), lh)]);
    std::fs::write(dir.join("grand.musicxml"), musicxml::write(&g)).unwrap();
    std::fs::write(dir.join("grand.svg"), ferraus_score::svg::render(&g)).unwrap();
}

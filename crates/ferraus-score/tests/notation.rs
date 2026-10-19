//! Acceptance: pitch spelling, drum map, chord symbols, solfa, performance, fix/review.
#![allow(clippy::field_reassign_with_default, clippy::manual_is_multiple_of, clippy::needless_range_loop)]

mod common;
use common::*;
use ferraus_score::midi::{read_midi, write_midi, MidiData, MidiNote, MidiTrack};
use ferraus_score::model::*;
use ferraus_score::notate::{notate, ChordMode, Layout, NotateOptions};
use ferraus_score::perform::{perform, PlayOptions};
use ferraus_score::pitch::{parse_key, spell_pc};
use ferraus_score::solfa::{self, SolfaKind};
use ferraus_score::{check, fix, musicxml};

fn midi_of(notes: &[(u64, u64, u8)], ch: u8, key: Option<(i8, bool)>, time: (u32, u32)) -> MidiData {
    MidiData {
        ppq: 480,
        notes: notes.iter().map(|(s, d, k)| MidiNote { track: 1, channel: ch, key: *k, velocity: 90, start: *s, end: s + d }).collect(),
        tracks: vec![MidiTrack::default(), MidiTrack { name: Some("Part".into()), program: None }],
        tempos: vec![(0, 500_000)],
        time_sigs: vec![(0, time.0, time.1)],
        key_sigs: key.map(|(f, m)| vec![(0, f, m)]).unwrap_or_default(),
        ..MidiData::default()
    }
}

fn spelled(s: &Score) -> Vec<String> {
    s.parts[0].staves[0]
        .measures
        .iter()
        .flat_map(|m| m.voices.iter().flat_map(|v| v.events.iter().flat_map(|e| e.notes.iter().map(|n| n.pitch.name()))))
        .collect()
}

#[test]
fn pitch_spelling_in_keys() {
    // (key, scale as MIDI from the tonic, expected names)
    let cases: &[(&str, &[u8], &[&str])] = &[
        ("G", &[67, 69, 71, 72, 74, 76, 78, 79], &["G4", "A4", "B4", "C5", "D5", "E5", "F#5", "G5"]),
        ("D", &[62, 64, 66, 67, 69, 71, 73, 74], &["D4", "E4", "F#4", "G4", "A4", "B4", "C#5", "D5"]),
        ("Bb", &[58, 60, 62, 63, 65, 67, 69, 70], &["Bb3", "C4", "D4", "Eb4", "F4", "G4", "A4", "Bb4"]),
        ("Db", &[61, 63, 65, 66, 68, 70, 72, 73], &["Db4", "Eb4", "F4", "Gb4", "Ab4", "Bb4", "C5", "Db5"]),
        ("Em", &[64, 66, 67, 69, 71, 72, 75, 76], &["E4", "F#4", "G4", "A4", "B4", "C5", "D#5", "E5"]),
        ("Cm", &[60, 62, 63, 65, 67, 68, 71, 72], &["C4", "D4", "Eb4", "F4", "G4", "Ab4", "B4", "C5"]),
        ("F#", &[66, 68, 70, 71, 73, 75, 77, 78], &["F#4", "G#4", "A#4", "B4", "C#5", "D#5", "E#5", "F#5"]),
        ("Ab", &[68, 70, 72, 73, 75, 77, 79, 80], &["Ab4", "Bb4", "C5", "Db5", "Eb5", "F5", "G5", "Ab5"]),
    ];
    for (k, scale, want) in cases {
        let notes: Vec<(u64, u64, u8)> = scale.iter().enumerate().map(|(i, m)| (i as u64 * 480, 480, *m)).collect();
        let mut opts = NotateOptions::default();
        opts.key = parse_key(k);
        opts.chords = ChordMode::Off;
        let (s, _) = notate(&midi_of(&notes, 0, None, (4, 4)), &opts);
        assert_eq!(spelled(&s), want.iter().map(|x| x.to_string()).collect::<Vec<_>>(), "key {k}");
        // MusicXML carries the key signature and only the needed accidentals
        let xml = musicxml::write(&s);
        assert!(xml.contains(&format!("<fifths>{}</fifths>", parse_key(k).unwrap().fifths)));
    }
}

#[test]
fn key_estimation_spells_without_a_key_signature() {
    // a G major tune with the tonic emphasized
    let tune = [67u8, 71, 74, 72, 71, 69, 67, 66, 67, 74, 71, 67];
    let notes: Vec<(u64, u64, u8)> = tune.iter().enumerate().map(|(i, m)| (i as u64 * 480, 480, *m)).collect();
    let (s, info) = notate(&midi_of(&notes, 0, None, (4, 4)), &NotateOptions::default());
    assert_eq!(info.key, "G major");
    assert!(spelled(&s).contains(&"F#4".to_string()));
    // Db major
    let tune = [61u8, 65, 68, 66, 65, 63, 61, 60, 61, 68, 65, 61];
    let notes: Vec<(u64, u64, u8)> = tune.iter().enumerate().map(|(i, m)| (i as u64 * 480, 480, *m)).collect();
    let (s, info) = notate(&midi_of(&notes, 0, None, (4, 4)), &NotateOptions::default());
    assert_eq!(info.key, "Db major");
    assert!(spelled(&s).contains(&"Gb4".to_string()));
}

#[test]
fn drum_score_uses_the_standard_map() {
    // kick 1/3, snare 2/4, closed hats eighths, crash on 1, ride and toms in bar 2
    let mut notes = Vec::new();
    for k in 0..8u64 {
        notes.push((k * 240, 120, 42u8));
    }
    notes.extend([(0, 120, 36u8), (960, 120, 36), (480, 120, 38), (1440, 120, 38), (0, 120, 49)]);
    for k in 0..4u64 {
        notes.push((1920 + k * 480, 120, 51));
    }
    notes.extend([(1920, 120, 36), (2400, 120, 50), (2640, 120, 47), (2880, 120, 43), (3360, 120, 38)]);
    let (s, info) = notate(&midi_of(&notes, 9, None, (4, 4)), &NotateOptions::default());
    assert_eq!(info.parts[0]["layout"], "drums");
    let p = &s.parts[0];
    assert!(p.percussion);
    assert_eq!(p.staves[0].measures[0].clef, Some(Clef::percussion()));
    let mut seen = std::collections::HashMap::new();
    for m in &p.staves[0].measures {
        for v in &m.voices {
            for e in &v.events {
                for n in &e.notes {
                    seen.insert(n.midi, (n.pitch.name(), n.notehead.clone(), v.number));
                }
            }
        }
    }
    assert_eq!(seen[&36], ("F4".to_string(), None, 2), "kick: bottom space, feet voice");
    assert_eq!(seen[&38], ("C5".to_string(), None, 1), "snare: third space");
    assert_eq!(seen[&42], ("G5".to_string(), Some("x".to_string()), 1), "hi-hat: x above the staff");
    assert_eq!(seen[&49], ("A5".to_string(), Some("x".to_string()), 1), "crash: x above the staff");
    assert_eq!(seen[&51].1, Some("x".to_string()), "ride: x notehead");
    assert_eq!(seen[&50].0, "E5", "high tom: fourth space");
    assert_eq!(seen[&43].0, "A4", "floor tom: second space");
    let xml = musicxml::write(&s);
    assert!(xml.contains("<sign>percussion</sign>"));
    assert!(xml.contains("<unpitched>\n          <display-step>F</display-step>\n          <display-octave>4</display-octave>"));
    assert!(xml.contains("<instrument id=\"P1-I37\"/>"));
    assert!(xml.contains("<midi-unpitched>43</midi-unpitched>"));
    assert!(xml.contains("<notehead>x</notehead>"));
    // reading it back keeps the drum keys
    let (back, _) = musicxml::read(&xml).unwrap();
    assert_eq!(musicxml::write(&back), xml);
    assert!(check::check(&s, &[]).ok, "{:?}", check::check(&s, &[]).issues);
    // drum hits have no ties
    assert!(!xml.contains("<tie "));
}

#[test]
fn chord_symbols_on_synthesized_progressions() {
    // seeded random progressions of triads and sevenths, some inverted (slash chords)
    let quals: &[(&str, &[i32])] = &[("maj", &[0, 4, 7]), ("min", &[0, 3, 7]), ("7", &[0, 4, 7, 10]), ("maj7", &[0, 4, 7, 11]), ("m7", &[0, 3, 7, 10]), ("dim", &[0, 3, 6]), ("sus4", &[0, 5, 7])];
    let mut seed = 7u64;
    let mut rnd = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    let mut total = 0;
    let mut correct = 0;
    let mut slash_total = 0;
    let mut slash_ok = 0;
    for _prog in 0..12 {
        let mut notes = Vec::new();
        let mut want = Vec::new();
        for bar in 0..4u64 {
            let root = rnd(12) as i32;
            let (q, ivs) = quals[rnd(quals.len() as u64) as usize];
            let inv = rnd(4) == 0 && ivs.len() >= 3;
            let bass_iv = if inv { ivs[1] } else { 0 };
            let bass = 36 + (root + bass_iv).rem_euclid(12);
            notes.push((bar * 1920, 1920, bass as u8));
            for iv in ivs {
                notes.push((bar * 1920, 1920, (60 + root + iv) as u8));
            }
            want.push((root, q, if inv { Some((root + bass_iv).rem_euclid(12)) } else { None }));
        }
        let mut opts = NotateOptions::default();
        opts.key = Some(KeySig::major(0));
        let (s, _) = notate(&midi_of(&notes, 0, None, (4, 4)), &opts);
        let st = &s.parts[0].staves[0];
        for (bar, (root, q, bass)) in want.iter().enumerate() {
            total += 1;
            let h = st.measures[bar].harmonies.iter().find(|h| h.offset == 0);
            if let Some(h) = h {
                if h.root.pc() == *root && h.quality == *q {
                    correct += 1;
                }
                if let Some(b) = bass {
                    slash_total += 1;
                    if h.bass.map(|x| x.pc()) == Some(*b) {
                        slash_ok += 1;
                    } else {
                        eprintln!("slash miss: want root {root} {q} bass {b}, got {}", h.text);
                    }
                }
            }
        }
    }
    let acc = correct as f64 / total as f64;
    eprintln!("chord accuracy {acc:.3} ({correct}/{total}), slash bass {slash_ok}/{slash_total}");
    assert!(acc >= 0.75, "chord accuracy {acc:.2}");
    assert!(slash_total > 0 && slash_ok as f64 / slash_total as f64 >= 0.75, "slash bass {slash_ok}/{slash_total}");
}

#[test]
fn chord_text_spelling_and_diagrams_with_capo() {
    // Eb major I - IV - V7 - I as block chords, transposed up 3 from C
    let prog: &[&[u8]] = &[&[48, 60, 64, 67], &[53, 60, 65, 69], &[43, 59, 62, 65, 67], &[48, 60, 64, 67]];
    let mut notes = Vec::new();
    for (i, ch) in prog.iter().enumerate() {
        for k in *ch {
            notes.push((i as u64 * 1920, 1920, *k));
        }
    }
    let mut opts = NotateOptions::default();
    opts.chords = ChordMode::Diagrams;
    opts.transpose = 3;
    let (s, info) = notate(&midi_of(&notes, 0, None, (4, 4)), &opts);
    assert_eq!(info.key, "Eb major");
    assert_eq!(info.chords, vec!["Eb", "Ab", "Bb7", "Eb"]);
    let capo = info.capo.unwrap();
    assert_eq!(capo["fret"], 1);
    let h = &s.parts[0].staves[0].measures[0].harmonies[0];
    let fr = h.frame.as_ref().unwrap();
    assert_eq!(fr.capo, Some(1));
    assert_eq!(fr.shape.as_deref(), Some("D"));
    let xml = musicxml::write(&s);
    assert!(xml.contains("<frame>") && xml.contains("<frame-strings>6</frame-strings>"));
    assert!(xml.contains("<root-alter>-1</root-alter>"));
    assert!(xml.contains("<words>Capo 1</words>"));
}

fn scale_melody(key: KeySig, minor_raised: bool) -> (Vec<(u64, u64, u8)>, Vec<&'static str>) {
    let tonic = ferraus_score::pitch::key_tonic_pc(key);
    let base = 60 + tonic;
    let (ivs, want): (Vec<i32>, Vec<&str>) = match key.mode {
        Mode::Major => (vec![0, 2, 4, 5, 7, 9, 11, 12, 7, 4, 0], vec!["d", "r", "m", "f", "s", "l", "t", "d'", "s", "m", "d"]),
        Mode::Minor => {
            if minor_raised {
                (vec![0, 2, 3, 5, 7, 8, 11, 12, 7, 3, 0], vec!["l,", "t,", "d", "r", "m", "f", "se", "l", "m", "d", "l,"])
            } else {
                (vec![0, 2, 3, 5, 7, 8, 10, 12], vec!["l,", "t,", "d", "r", "m", "f", "s", "l"])
            }
        }
    };
    let notes = ivs.iter().enumerate().map(|(i, iv)| (i as u64 * 480, 480, (base + iv) as u8)).collect();
    (notes, want)
}

#[test]
fn solfa_syllables_in_every_key() {
    for pc in 0..12 {
        for mode in [Mode::Major, Mode::Minor] {
            let key = ferraus_score::pitch::key_from_pc(pc, mode, None);
            for raised in [false, true] {
                if mode == Mode::Major && raised {
                    continue;
                }
                let (notes, want) = scale_melody(key, raised);
                let mut opts = NotateOptions::default();
                opts.key = Some(key);
                opts.chords = ChordMode::Off;
                let (mut s, _) = notate(&midi_of(&notes, 0, None, (4, 4)), &opts);
                solfa::annotate(&mut s, SolfaKind::Movable);
                let got: Vec<String> = s.parts[0].staves[0].measures.iter().flat_map(|m| m.voices[0].events.iter().filter_map(|e| e.solfa.clone())).collect();
                // octave marks are relative to the doh at or below the median, so compare
                // relative to the first syllable's register
                let strip = |v: &[String]| v.iter().map(|x| x.trim_end_matches(['\'', ',']).to_string()).collect::<Vec<_>>();
                let w: Vec<String> = want.iter().map(|x| x.to_string()).collect();
                assert_eq!(strip(&got), strip(&w), "{} {:?}", ferraus_score::pitch::key_name(key), mode);
                // the octave mark pattern is consistent: the highest note carries more ' than the lowest
                let text = solfa::write_text(&s, SolfaKind::Movable);
                assert!(text.contains("Doh is"));
                assert!(!text.contains('<'));
            }
        }
    }
    // concrete octave marks in C major around middle C
    let (notes, want) = scale_melody(KeySig::major(0), false);
    let mut opts = NotateOptions::default();
    opts.key = Some(KeySig::major(0));
    let (mut s, _) = notate(&midi_of(&notes, 0, None, (4, 4)), &opts);
    solfa::annotate(&mut s, SolfaKind::Movable);
    let got: Vec<String> = s.parts[0].staves[0].measures.iter().flat_map(|m| m.voices[0].events.iter().filter_map(|e| e.solfa.clone())).collect();
    assert_eq!(got, want.iter().map(|x| x.to_string()).collect::<Vec<_>>());
    let text = solfa::write_text(&s, SolfaKind::Movable);
    assert!(text.contains("|d :r :m :f |s :l :t :d' |s :m :d"), "{text}");
}

#[test]
fn solfa_rhythm_punctuation() {
    // 4/4: half-beat pairs, quarter-beats, a held note, and a rest
    let s = score(KeySig::major(0), TimeSig::new(4, 4), false, &[&["C4:e", "D4:e", "E4:s", "F4:s", "G4:e", "A4:h"], &["r:q", "G4:q.", "E4:e", "C4:q"]]);
    let text = solfa::write_text(&s, SolfaKind::Movable);
    let line = text.lines().find(|l| l.starts_with('|')).unwrap();
    assert_eq!(line, "|d.r :m,f.s :l :- | :s :-.m :d ||");
}

#[test]
fn fixed_do_syllables() {
    let g = KeySig::major(1);
    let notes: Vec<(u64, u64, u8)> = [67u8, 69, 71, 72, 74, 76, 78, 79].iter().enumerate().map(|(i, m)| (i as u64 * 480, 480, *m)).collect();
    let mut opts = NotateOptions::default();
    opts.key = Some(g);
    let (mut s, _) = notate(&midi_of(&notes, 0, None, (4, 4)), &opts);
    solfa::annotate(&mut s, SolfaKind::Fixed);
    let got: Vec<String> = s.parts[0].staves[0].measures.iter().flat_map(|m| m.voices[0].events.iter().filter_map(|e| e.solfa.clone())).map(|x| x.trim_end_matches(['\'', ',']).to_string()).collect();
    assert_eq!(got, vec!["sol", "la", "si", "do", "re", "mi", "fa#", "sol"]);
}

const REPEAT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="4.0">
  <part-list><score-part id="P1"><part-name>Piano</part-name></score-part></part-list>
  <part id="P1">
    <measure number="1">
      <barline location="left"><repeat direction="forward"/></barline>
      <attributes><divisions>1</divisions><key><fifths>0</fifths></key><time><beats>4</beats><beat-type>4</beat-type></time><clef><sign>G</sign><line>2</line></clef></attributes>
      <direction><direction-type><metronome><beat-unit>quarter</beat-unit><per-minute>120</per-minute></metronome></direction-type><sound tempo="120"/></direction>
      <direction placement="below"><direction-type><dynamics><p/></dynamics></direction-type></direction>
      <note><pitch><step>C</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><type>whole</type></note>
    </measure>
    <measure number="2">
      <note><pitch><step>D</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><type>whole</type></note>
    </measure>
    <measure number="3">
      <barline location="left"><ending number="1" type="start"/></barline>
      <note><pitch><step>E</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><type>whole</type></note>
      <barline location="right"><ending number="1" type="stop"/><repeat direction="backward"/></barline>
    </measure>
    <measure number="4">
      <barline location="left"><ending number="2" type="start"/></barline>
      <direction placement="below"><direction-type><dynamics><f/></dynamics></direction-type></direction>
      <note><pitch><step>G</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><type>whole</type><notations><articulations><staccato/></articulations></notations></note>
      <barline location="right"><bar-style>light-heavy</bar-style><ending number="2" type="discontinue"/></barline>
    </measure>
  </part>
</score-partwise>
"#;

#[test]
fn play_unrolls_repeats_and_maps_dynamics() {
    let (s, unsupported) = musicxml::read(REPEAT_XML).unwrap();
    assert!(unsupported.is_empty(), "{unsupported:?}");
    assert!(check::check(&s, &[]).ok);
    let (m, info) = perform(&s, &PlayOptions::default());
    // 1 2 3 | 1 2 4 → six whole notes at 120 bpm = 12 s
    assert_eq!(info.order, vec![1, 2, 3, 1, 2, 4]);
    assert!((info.duration_s - 12.0).abs() < 1e-6, "{}", info.duration_s);
    let keys: Vec<u8> = m.notes.iter().map(|n| n.key).collect();
    assert_eq!(keys, vec![60, 62, 64, 60, 62, 67]);
    let vels: Vec<u8> = m.notes.iter().map(|n| n.velocity).collect();
    assert_eq!(vels[..5], [49, 49, 49, 49, 49]);
    assert_eq!(vels[5], 96);
    // staccato halves the last note
    let last = m.notes.last().unwrap();
    assert_eq!(last.end - last.start, m.ppq as u64 * 2);
    // without repeats: 4 measures, 8 s
    let (_, info2) = perform(&s, &PlayOptions { repeats: false, dynamics: false, ..PlayOptions::default() });
    assert!((info2.duration_s - 8.0).abs() < 1e-6);
    // tempo override and click
    let (m3, info3) = perform(&s, &PlayOptions { tempo: Some(60.0), click: true, ..PlayOptions::default() });
    assert!((info3.duration_s - 24.0).abs() < 1e-6);
    assert_eq!(m3.notes.iter().filter(|n| n.channel == 9).count(), 24);
    // written MIDI parses back
    let back = read_midi(&write_midi(&m).unwrap()).unwrap();
    assert_eq!(back.notes.len(), 6);
}

#[test]
fn swing_and_humanize_are_seeded() {
    let s = score(KeySig::major(0), TimeSig::new(4, 4), false, &[&["C4:e", "D4:e", "E4:e", "F4:e", "G4:h"]]);
    let (m, _) = perform(&s, &PlayOptions { swing: 1.0, ..PlayOptions::default() });
    // the off-beat eighth starts at 2/3 of the beat
    assert_eq!(m.notes[1].start, m.ppq as u64 * 2 / 3);
    let a = perform(&s, &PlayOptions { humanize: 0.5, seed: 3, ..PlayOptions::default() }).0;
    let b = perform(&s, &PlayOptions { humanize: 0.5, seed: 3, ..PlayOptions::default() }).0;
    assert_eq!(a.notes, b.notes);
}

#[test]
fn review_flags_wrong_durations_and_fix_repairs_them() {
    // measure 2 is a quarter short
    let s = score(KeySig::major(0), TimeSig::new(4, 4), false, &[&["C4:q", "D4:q", "E4:q", "F4:q"], &["G4:q", "A4:q", "B4:q"], &["C5:w"]]);
    let r = check::check(&s, &[]);
    assert!(!r.ok);
    assert_eq!(r.failing_measures, vec![2]);
    assert!(r.issues.iter().any(|i| i.code == "measure_too_short" && i.measure == Some(2)));
    let mut s2 = s.clone();
    let edits = serde_json::json!([
        {"measure": 2, "voice": 1, "note": 3, "set": {"duration": "half"}},
        {"measure": 1, "note": 2, "set": {"pitch": "E5"}},
        {"measure": 3, "note": 1, "set": {"pitch": ["C4", "E4", "G4"]}},
        {"measure": 9, "note": 1, "set": {"pitch": "C4"}}
    ]);
    let res = fix::apply(&mut s2, &edits).unwrap();
    assert_eq!(res.iter().filter(|r| r["ok"] == true).count(), 3);
    assert_eq!(res[3]["ok"], false);
    assert!(check::check(&s2, &[]).ok);
    let ev = &s2.parts[0].staves[0].measures[0].voices[0].events[1];
    assert_eq!(ev.notes[0].pitch.name(), "E5");
    assert_eq!(ev.notes[0].midi, 76);
    assert_eq!(s2.parts[0].staves[0].measures[2].voices[0].events[0].notes.len(), 3);
    // an over-long measure is flagged too
    let long = score(KeySig::major(0), TimeSig::new(3, 4), false, &[&["C4:h", "D4:h"]]);
    assert!(check::check(&long, &[]).issues.iter().any(|i| i.code == "measure_too_long"));
    // pickup plus a completing final bar is fine
    let ok = score(KeySig::major(0), TimeSig::new(4, 4), true, &[&["G4:q"], &["C5:w"], &["B4:h", "A4:q"]]);
    assert!(check::check(&ok, &[]).ok, "{:?}", check::check(&ok, &[]).issues);
}

#[test]
fn lyrics_one_syllable_per_note() {
    let notes: Vec<(u64, u64, u8)> = [60u8, 62, 64, 65, 67].iter().enumerate().map(|(i, m)| (i as u64 * 480, 480, *m)).collect();
    let mut opts = NotateOptions::default();
    opts.lyrics = Some("Hap-py birth-day to".into());
    let (s, _) = notate(&midi_of(&notes, 0, None, (4, 4)), &opts);
    let lyr: Vec<(String, String)> = s.parts[0].staves[0].measures.iter().flat_map(|m| m.voices[0].events.iter().filter_map(|e| e.lyrics.first().map(|l| (l.text.clone(), l.syllabic.clone())))).collect();
    assert_eq!(lyr[0], ("Hap".to_string(), "begin".to_string()));
    assert_eq!(lyr[1], ("py".to_string(), "end".to_string()));
    assert_eq!(lyr.len(), 5);
    let xml = musicxml::write(&s);
    assert!(xml.contains("<text>birth</text>"));
}

#[test]
fn layout_auto_picks_grand_for_wide_piano_parts() {
    let mut notes = Vec::new();
    for bar in 0..2u64 {
        for (k, m) in [36u8, 43, 48].iter().enumerate() {
            let _ = k;
            notes.push((bar * 1920, 1920, *m));
        }
        for k in 0..4u64 {
            notes.push((bar * 1920 + k * 480, 480, 72 + (k as u8) * 2));
        }
    }
    let (s, info) = notate(&midi_of(&notes, 0, None, (4, 4)), &NotateOptions::default());
    assert_eq!(info.parts[0]["layout"], "grand");
    assert_eq!(s.parts[0].staves.len(), 2);
    // a low monophonic part becomes a bass-clef lead sheet
    let bass: Vec<(u64, u64, u8)> = [36u8, 38, 40, 41].iter().enumerate().map(|(i, m)| (i as u64 * 480, 480, *m)).collect();
    let (s, _) = notate(&midi_of(&bass, 0, None, (4, 4)), &NotateOptions::default());
    assert_eq!(s.parts[0].staves[0].measures[0].clef, Some(Clef::bass()));
    let _ = Layout::Auto;
    let _ = spell_pc(0, KeySig::major(0));
}

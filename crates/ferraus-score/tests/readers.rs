//! Readers on hand-written files in the styles other programs produce.

use ferraus_score::check::check;
use ferraus_score::model::*;
use ferraus_score::perform::{perform, PlayOptions};
use ferraus_score::{abc, lily, musicxml};

/// MuseScore-style MusicXML: print/beam/stem noise, two voices with backup and forward,
/// grace notes, slurs, lyrics with extend, dynamics with sound, chord symbols with offset.
const MUSESCORE_STYLE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<!DOCTYPE score-partwise PUBLIC "-//Recordare//DTD MusicXML 4.0 Partwise//EN" "http://www.musicxml.org/dtds/partwise.dtd">
<score-partwise version="4.0">
  <work><work-title>Study</work-title></work>
  <identification><creator type="composer">Anon</creator><encoding><software>MuseScore 4.2</software><supports element="accidental" type="yes"/></encoding></identification>
  <defaults><scaling><millimeters>7</millimeters><tenths>40</tenths></scaling></defaults>
  <part-list>
    <score-part id="P1"><part-name>Flute</part-name><score-instrument id="P1-I1"><instrument-name>Flute</instrument-name></score-instrument><midi-instrument id="P1-I1"><midi-channel>1</midi-channel><midi-program>74</midi-program><volume>78.7</volume></midi-instrument></score-part>
  </part-list>
  <part id="P1">
    <measure number="1" width="300">
      <print><system-layout><system-margins><left-margin>50</left-margin></system-margins></system-layout></print>
      <attributes><divisions>2</divisions><key><fifths>-1</fifths><mode>major</mode></key><time><beats>3</beats><beat-type>4</beat-type></time><clef><sign>G</sign><line>2</line></clef></attributes>
      <direction placement="above"><direction-type><metronome parentheses="no"><beat-unit>quarter</beat-unit><per-minute>90</per-minute></metronome></direction-type><sound tempo="90"/></direction>
      <harmony print-frame="no"><root><root-step>F</root-step></root><kind text="">major</kind></harmony>
      <direction placement="below"><direction-type><dynamics><mp/></dynamics></direction-type><sound dynamics="71"/></direction>
      <note default-x="80"><grace slash="yes"/><pitch><step>G</step><octave>4</octave></pitch><voice>1</voice><type>eighth</type><stem>up</stem></note>
      <note default-x="100"><pitch><step>F</step><octave>4</octave></pitch><duration>2</duration><voice>1</voice><type>quarter</type><stem>up</stem><notations><slur type="start" number="1"/></notations><lyric number="1"><syllabic>single</syllabic><text>Ah</text><extend/></lyric></note>
      <note><pitch><step>A</step><octave>4</octave></pitch><duration>1</duration><voice>1</voice><type>eighth</type><stem>up</stem><beam number="1">begin</beam></note>
      <note><pitch><step>B</step><alter>-1</alter><octave>4</octave></pitch><duration>1</duration><voice>1</voice><type>eighth</type><stem>up</stem><beam number="1">end</beam><notations><slur type="stop" number="1"/></notations></note>
      <harmony><root><root-step>C</root-step></root><kind text="7">dominant</kind><bass><bass-step>E</bass-step></bass><offset>0</offset></harmony>
      <note><pitch><step>C</step><octave>5</octave></pitch><duration>2</duration><voice>1</voice><type>quarter</type><stem>up</stem><notations><articulations><staccato/></articulations></notations></note>
      <backup><duration>6</duration></backup>
      <forward><duration>2</duration></forward>
      <note><pitch><step>D</step><octave>4</octave></pitch><duration>4</duration><voice>2</voice><type>half</type><stem>down</stem></note>
    </measure>
    <measure number="2" width="200">
      <note><pitch><step>F</step><octave>5</octave></pitch><duration>6</duration><voice>1</voice><type>half</type><dot/><stem>down</stem><notations><fermata type="upright"/></notations></note>
      <barline location="right"><bar-style>light-heavy</bar-style></barline>
    </measure>
  </part>
</score-partwise>
"#;

#[test]
fn reads_musescore_style_musicxml() {
    let (s, unsupported) = musicxml::read(MUSESCORE_STYLE).unwrap();
    assert!(unsupported.is_empty(), "{unsupported:?}");
    assert_eq!(s.title.as_deref(), Some("Study"));
    assert_eq!(s.composer.as_deref(), Some("Anon"));
    let p = &s.parts[0];
    assert_eq!(p.program, Some(73));
    let m1 = &p.staves[0].measures[0];
    assert_eq!(m1.key, Some(KeySig::major(-1)));
    assert_eq!(m1.time, Some(TimeSig::new(3, 4)));
    assert_eq!(m1.tempo[0].bpm, 90.0);
    assert_eq!(m1.voices.len(), 2);
    let v1 = &m1.voices[0].events;
    assert!(v1[0].grace);
    assert_eq!(v1[1].dynamic.as_deref(), Some("mp"));
    assert_eq!(v1[1].lyrics[0].text, "Ah");
    assert_eq!(v1[1].slurs, vec!["start"]);
    assert_eq!(v1[3].notes[0].pitch.name(), "Bb4");
    assert_eq!(v1[4].articulations, vec!["staccato"]);
    // voice 2 starts after a forward: a hidden rest fills the gap
    let v2 = &m1.voices[1].events;
    assert!(v2[0].is_rest() && !v2[0].visible);
    assert_eq!(v2[1].notes[0].pitch.name(), "D4");
    let h: Vec<(u32, String)> = m1.harmonies.iter().map(|h| (h.offset, h.text.clone())).collect();
    assert_eq!(h, vec![(0, "F".to_string()), (4, "C7/E".to_string())]);
    assert_eq!(p.staves[0].measures[1].voices[0].events[0].articulations, vec!["fermata"]);
    assert!(check(&s, &[]).ok, "{:?}", check(&s, &[]).issues);
    // normalized output is stable
    let x1 = musicxml::write(&s);
    let (s2, _) = musicxml::read(&x1).unwrap();
    assert_eq!(musicxml::write(&s2), x1);
    // performance: grace notes are skipped, the measures last 3 + 3 beats at 90 bpm (the
    // fermata lets the last note ring past the bar)
    let (m, info) = perform(&s, &PlayOptions::default());
    assert!((m.tick_to_seconds(m.end_tick) - 4.0).abs() < 1e-4);
    assert!(info.duration_s > 4.0);
    assert_eq!(m.tracks[0].program.map(|p| p.1), Some(73));
}

const ABC_TUNE: &str = r#"X:1
T:The Test Reel
C:Trad.
M:4/4
L:1/8
Q:1/4=120
K:D
|:"D"A2 FA dAFA|"G"B2 GB "A7"(3ABc d2|1"D"d2 fd A2 FA:|2"D"d2 dc d4|]
w:la la la la la la la la la la la la la la la la la la
"#;

#[test]
fn reads_a_typical_abc_tune() {
    let s = abc::read(ABC_TUNE).unwrap();
    assert_eq!(s.title.as_deref(), Some("The Test Reel"));
    let st = &s.parts[0].staves[0];
    assert_eq!(st.measures.len(), 4);
    assert_eq!(Score::key_at(st, 0), KeySig::major(2));
    assert!(st.measures[0].repeat_start);
    assert_eq!(st.measures[2].repeat_end, Some(2));
    assert_eq!(st.measures[2].ending.as_ref().unwrap().number, "1");
    assert_eq!(st.measures[3].ending.as_ref().unwrap().number, "2");
    // F is sharp in D major
    let n = &st.measures[0].voices[0].events[1].notes[0];
    assert_eq!(n.pitch.name(), "F#4");
    // a triplet in measure 2
    assert!(st.measures[1].voices[0].events.iter().any(|e| e.tuplet.is_some()));
    let h: Vec<String> = st.measures.iter().flat_map(|m| m.harmonies.iter().map(|h| h.text.clone())).collect();
    assert_eq!(h, vec!["D", "G", "A7", "D", "D"]);
    assert!(check(&s, &[]).ok, "{:?}", check(&s, &[]).issues);
    // 1 2 3 | 1 2 4 with repeats
    let (_, info) = perform(&s, &PlayOptions::default());
    assert_eq!(info.order, vec![1, 2, 3, 1, 2, 4]);
    // lyrics attach to notes
    assert_eq!(st.measures[0].voices[0].events[0].lyrics[0].text, "la");
}

#[test]
fn abc_broken_rhythm_and_lengths() {
    let s = abc::read("X:1\nT:t\nM:2/4\nL:1/8\nK:C\nA>B c/d/e | z2 C,2 |]\n").unwrap();
    let evs = &s.parts[0].staves[0].measures[0].voices[0].events;
    let durs: Vec<u32> = evs.iter().map(|e| e.duration * 4 / s.divisions).collect();
    // in sixteenths: 3 1 1 1 2
    assert_eq!(durs, vec![3, 1, 1, 1, 2]);
    let m2 = &s.parts[0].staves[0].measures[1].voices[0].events;
    assert!(m2[0].is_rest());
    assert_eq!(m2[1].notes[0].pitch.name(), "C3");
}

#[test]
fn reads_hand_written_lilypond() {
    let src = r#"\version "2.24.0"
\header { title = "Little" }
\score {
  \new Staff {
    \clef treble \key g \major \time 3/4
    \partial 4 d'4 |
    g'4 b'8 a'8 g'4 | fis'2. ~ | fis'2 r4 |
    \tuplet 3/2 { g'8 a' b' } c''2 \bar "|."
  }
}
"#;
    let s = lily::read(src).unwrap();
    let st = &s.parts[0].staves[0];
    assert!(st.measures[0].implicit);
    assert_eq!(Score::key_at(st, 0), KeySig::major(1));
    assert_eq!(Score::time_at(st, 0), TimeSig::new(3, 4));
    let names: Vec<String> = st
        .measures
        .iter()
        .flat_map(|m| m.voices[0].events.iter().flat_map(|e| e.notes.iter().map(|n| n.pitch.name())))
        .collect();
    assert_eq!(names, vec!["D4", "G4", "B4", "A4", "G4", "F#4", "F#4", "G4", "A4", "B4", "C5"]);
    assert!(st.measures[2].voices[0].events[0].notes[0].tie_start);
    assert!(st.measures[3].voices[0].events[0].notes[0].tie_stop);
    assert!(st.measures[4].voices[0].events[0].tuplet.is_some());
    assert_eq!(st.measures.last().unwrap().barline.as_deref(), Some("light-heavy"));
    assert!(check(&s, &[]).ok, "{:?}", check(&s, &[]).issues);
}

#[test]
fn timewise_and_garbage_are_rejected_cleanly() {
    let e = musicxml::read("<score-timewise version=\"4.0\"></score-timewise>").unwrap_err();
    assert_eq!(e.kind, ferraus_core::ErrorKind::Unsupported);
    let e = musicxml::read("not xml").unwrap_err();
    assert_eq!(e.kind, ferraus_core::ErrorKind::InputMissing);
    assert!(abc::read("just text").is_err());
    let e = ferraus_score::midi::read_midi(b"MThd garbage").unwrap_err();
    assert_eq!(e.kind, ferraus_core::ErrorKind::InputMissing);
}

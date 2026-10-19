//! The helper protocol: MusicXML on stdin, SVG/PDF/PNG on stdout.

use std::io::Write;
use std::process::{Command, Stdio};

const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="4.0">
  <part-list><score-part id="P1"><part-name>Piano</part-name></score-part></part-list>
  <part id="P1">
    <measure number="1">
      <attributes><divisions>1</divisions><key><fifths>1</fifths></key><time><beats>4</beats><beat-type>4</beat-type></time><clef><sign>G</sign><line>2</line></clef></attributes>
      <harmony><root><root-step>G</root-step></root><kind>major</kind></harmony>
      <note><pitch><step>G</step><octave>4</octave></pitch><duration>1</duration><voice>1</voice><type>quarter</type></note>
      <note><pitch><step>A</step><octave>4</octave></pitch><duration>1</duration><voice>1</voice><type>quarter</type></note>
      <note><pitch><step>B</step><octave>4</octave></pitch><duration>2</duration><voice>1</voice><type>half</type></note>
    </measure>
  </part>
</score-partwise>
"#;

fn run(args: &[&str], input: &str) -> (i32, Vec<u8>, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ferraus-engrave"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = c.stdin.take().unwrap().write_all(input.as_bytes());
    let o = c.wait_with_output().unwrap();
    (o.status.code().unwrap_or(-1), o.stdout, String::from_utf8_lossy(&o.stderr).to_string())
}

#[test]
fn usage_errors() {
    let (code, _, err) = run(&["--to", "gif"], XML);
    assert_eq!(code, 2, "{err}");
    let (code, _, _) = run(&[], XML);
    assert_eq!(code, 2);
    let (code, out, _) = run(&["--version"], "");
    assert_eq!(code, 0);
    assert!(String::from_utf8_lossy(&out).starts_with("ferraus-engrave "));
}

#[cfg(feature = "verovio")]
#[test]
fn renders_svg_pdf_png() {
    let (code, out, err) = run(&["--count"], XML);
    assert_eq!(code, 0, "{err}");
    assert_eq!(String::from_utf8_lossy(&out).trim(), "1");
    let (code, svg, err) = run(&["--to", "svg"], XML);
    assert_eq!(code, 0, "{err}");
    let svg = String::from_utf8(svg).unwrap();
    assert!(svg.contains("<svg") && svg.contains("class=\"note\""), "{}", &svg[..svg.len().min(300)]);
    let (code, pdf, err) = run(&["--to", "pdf"], XML);
    assert_eq!(code, 0, "{err}");
    assert!(pdf.starts_with(b"%PDF"));
    let (code, png, err) = run(&["--to", "png", "--scale", "0.5"], XML);
    assert_eq!(code, 0, "{err}");
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    let (code, _, _) = run(&["--to", "svg", "--page", "9"], XML);
    assert_eq!(code, 2);
    let (code, _, _) = run(&["--to", "svg"], "not music");
    assert_eq!(code, 3);
}

#[cfg(not(feature = "verovio"))]
#[test]
fn without_verovio_reports_unsupported() {
    let (code, _, _) = run(&["--to", "svg"], XML);
    assert_eq!(code, 4);
}

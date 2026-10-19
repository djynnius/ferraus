mod common;
use common::*;
use ferraus_analysis::{loudness, stereo};
use ferraus_core::testing::{read_wav, TempDir};

#[test]
#[ignore]
fn explore_streaming() {
    let td = TempDir::new("explore");
    for s in styles() {
        let t0 = std::time::Instant::now();
        let b = song(&s);
        let inp = td.path(&format!("{}.wav", s.name));
        write(&inp, &b);
        let out = td.path(&format!("{}.master.wav", s.name));
        let o = invoke("master", &[&inp], &[], Some(&out));
        if !o.envelope.ok {
            println!("{}: ERROR {:?}", s.name, o.envelope.error);
            continue;
        }
        let m = read_wav(&out);
        let l = loudness::loudness(&m);
        let st = stereo::stereo_analysis(&m);
        let r = &o.envelope.result;
        println!(
            "{:9} in {:6.2} LUFS -> {:6.2} LUFS tp {:5.2} corr {:.2} low {:.2} gr {} genre {} intensity {} warn {:?} ({:.1}s)",
            s.name,
            loudness::integrated_lufs(&b),
            l.integrated_lufs,
            l.true_peak_dbtp,
            st.correlation,
            st.correlation_below_120hz,
            r["loudness"]["limiter_gain_reduction_db"],
            r["genre"]["name"],
            r["intensity"]["name"],
            o.envelope.warnings.iter().map(|w| w.code.clone()).collect::<Vec<_>>(),
            t0.elapsed().as_secs_f64()
        );
        for st in r["plan"]["stages"].as_array().unwrap() {
            println!("    {} {}", st["op"], st["args"]);
        }
        println!("    notes {}", r["notes"]);
        let m = ferraus_analysis::mastering_analysis(&b, None);
        println!("    genre features {}", serde_json::to_string(&ferraus_master::genres::guess(&m).features).unwrap());
    }
}

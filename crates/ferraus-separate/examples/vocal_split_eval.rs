//! Evaluate the built-in lead/backing split on ground-truth directories.
//!
//! ```text
//! cargo run -p ferraus-separate --example vocal_split_eval -- DIR...
//! ```
//!
//! Each `DIR` holds `vocals.wav`, `lead.wav`, and `backing.wav` (`vocals = lead + backing`,
//! float WAV). Prints the SI-SDR (dB) of the estimated lead and backing, stereo and downmixed
//! to mono, next to the trivial baselines that call the whole stem the lead (`base`) or the
//! backing (`bbase`). `VS_<FIELD>=value` overrides a field of `vocal_split::Options` (e.g.
//! `VS_STEREO_POWER=0`). Other switches:
//!
//! * `VS_TRACKER=crepe|swiftf0`: a model pitch tracker (`FERRAUS_AUTO_DOWNLOAD=1` fetches it);
//!   `VS_PYIN_DEFAULT=1` or `VS_PYIN_<FIELD>`: stock or tuned pYIN instead of `LeadPyin`.
//! * `VS_ORACLE=1`: track the pitch on the true lead (how much a better tracker could give).
//! * `VS_CLEAN=1`: the stem is the lead alone (how much of the lead the mask keeps).
//! * `VS_CENTER=1`: the stereo weighting alone, no pitch; `VS_IRM=1`: the ideal ratio mask.
//! * `VS_MODEL=<onnx>` (`VS_MODEL_SPEC=<registry name>`, default `separate/mdx-kara-2`): a
//!   lead/backing model through the separation adapters.
//! * `VS_STATS=1`: per-file voicing and backing energy on stderr.

use ferraus_core::AudioBuffer;
use ferraus_pitch::tracker::PyinTracker;
use ferraus_separate::stream::Track;
use ferraus_separate::vocal_split::{self, Options};

fn read(path: &std::path::Path) -> AudioBuffer {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    ferraus_core::wav::decode(&bytes).expect("wav").0
}

fn flat(b: &AudioBuffer, n: usize) -> Vec<f64> {
    let mut v = Vec::with_capacity(n * b.channels());
    for c in &b.data {
        v.extend((0..n).map(|i| c.get(i).copied().unwrap_or(0.0) as f64));
    }
    v
}

fn si_sdr(est: &[f64], reference: &[f64]) -> f64 {
    let rr: f64 = reference.iter().map(|r| r * r).sum();
    let er: f64 = est.iter().zip(reference).map(|(e, r)| e * r).sum();
    let a = er / rr.max(1e-20);
    let (mut t, mut d) = (0.0, 0.0);
    for (e, r) in est.iter().zip(reference) {
        let s = a * r;
        t += s * s;
        d += (e - s) * (e - s);
    }
    10.0 * (t.max(1e-20) / d.max(1e-20)).log10()
}

fn mono(b: &AudioBuffer) -> AudioBuffer {
    AudioBuffer::from_mono(b.sample_rate, b.mono_mix())
}

fn options() -> Options {
    let mut o = Options::default();
    let get = |k: &str| std::env::var(format!("VS_{k}")).ok().and_then(|v| v.parse::<f64>().ok());
    macro_rules! set {
        ($($f:ident => $k:literal),*) => { $( if let Some(v) = get($k) { o.$f = v; } )* };
    }
    set!(lobe_bins => "LOBE_BINS", tolerance_cents => "TOLERANCE_CENTS", comb_max_hz => "COMB_MAX_HZ",
        memory_s => "MEMORY_S", peak_min_db => "PEAK_MIN_DB", peak_full_db => "PEAK_FULL_DB",
        diffuse_gain => "DIFFUSE_GAIN", gap_gain => "GAP_GAIN", gap_max_hz => "GAP_MAX_HZ", stereo_diffuse => "STEREO_DIFFUSE", stereo_lo => "STEREO_LO", stereo_hi => "STEREO_HI", stereo_power => "STEREO_POWER", fmin => "FMIN", fmax => "FMAX");
    if let Some(v) = get("PEAK_LOBE_BINS") {
        o.peak_lobe_bins = v as usize;
    }
    if let Some(v) = get("FLOOR_BINS") {
        o.floor_bins = v as usize;
    }
    o
}

/// pYIN with `VS_PYIN_<FIELD>` overrides (beta_mean, yin_trust, voicing_stay, silence_db).
struct TunedPyin;

impl ferraus_pitch::tracker::PitchTracker for TunedPyin {
    fn name(&self) -> &str {
        "pyin"
    }
    fn track(
        &self,
        x: &[f32],
        sr: u32,
        t: &ferraus_pitch::tracker::TrackOptions,
    ) -> ferraus_core::Result<Vec<ferraus_pitch::pyin::PitchFrame>> {
        let get = |k: &str| std::env::var(format!("VS_PYIN_{k}")).ok().and_then(|v| v.parse::<f64>().ok());
        let mut o =
            ferraus_pitch::pyin::PyinOptions { fmin: t.fmin, fmax: t.fmax, hop_s: t.hop_s, ..Default::default() };
        if let Some(v) = get("BETA_MEAN") {
            o.beta_mean = v;
        }
        if let Some(v) = get("YIN_TRUST") {
            o.yin_trust = v;
        }
        if let Some(v) = get("VOICING_STAY") {
            o.voicing_stay = v;
        }
        if let Some(v) = get("SILENCE_DB") {
            o.silence_db = v;
        }
        if std::env::var("VS_PYIN_DEFAULT").is_ok() {
            return PyinTracker.track(x, sr, t);
        }
        Ok(ferraus_pitch::pyin::track(x, sr, &o))
    }
}

fn run(v: &AudioBuffer, oracle: &AudioBuffer, o: &Options) -> (AudioBuffer, AudioBuffer, vocal_split::Stats) {
    let sr = v.sample_rate;
    let src = Track::from_buffer(v.clone(), None).unwrap();
    // VS_ORACLE=1 tracks the pitch on the true lead (upper bound for the mask)
    let pitch_src = if std::env::var("VS_ORACLE").is_ok() {
        Track::from_buffer(oracle.clone(), None).unwrap()
    } else {
        Track::from_buffer(v.clone(), None).unwrap()
    };
    // VS_TRACKER=swiftf0|crepe uses a model tracker (FERRAUS_AUTO_DOWNLOAD=1 fetches it)
    let tracker: Box<dyn ferraus_pitch::tracker::PitchTracker> = match std::env::var("VS_TRACKER") {
        Ok(name) => {
            ferraus_ml::register_all();
            ferraus_pitch::tracker::by_name(&name).unwrap()
        }
        Err(_) if std::env::vars().any(|(k, _)| k.starts_with("VS_PYIN_")) => Box::new(TunedPyin),
        Err(_) => Box::new(vocal_split::LeadPyin),
    };
    let f0 = vocal_split::track_f0(tracker.as_ref(), &pitch_src, sr, o, &mut |_| Ok(())).unwrap();
    let mut lead = Track::zeros(src.frames, src.channels, None).unwrap();
    let mut back = Track::zeros(src.frames, src.channels, None).unwrap();
    let st = vocal_split::split(&src, sr, &f0, o, 1 << 18, &mut lead, &mut back, &mut |_| Ok(())).unwrap();
    (lead.to_buffer(sr).unwrap(), back.to_buffer(sr).unwrap(), st)
}

/// VS_MODEL=<onnx>: run a lead/backing model with the registry spec named by VS_MODEL_SPEC
/// (default separate/mdx-kara-2) instead of the built-in mask; its stems are mapped by name.
fn model_split(v: &AudioBuffer) -> (AudioBuffer, AudioBuffer, vocal_split::Stats) {
    use ferraus_separate::{adapter, pipeline, stream};
    let path = std::path::PathBuf::from(std::env::var("VS_MODEL").unwrap());
    let name = std::env::var("VS_MODEL_SPEC").unwrap_or_else(|_| "separate/mdx-kara-2".into());
    let manifest = ferraus_models::Manifest::embedded();
    let spec = ferraus_models::ModelSpec::from_entry(manifest.resolve(&name).unwrap());
    let (session, _) = ferraus_ml::session::load(&path, &Default::default()).unwrap();
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        eprintln!("inputs {:?} {:?}, outputs {:?}", session.input_names, session.input_shapes, session.output_names)
    });
    let stems = adapter::stems_of(&spec);
    let model = adapter::open(&name, spec, session).unwrap();
    let src = Track::from_buffer(v.clone(), None).unwrap();
    let mut outs: Vec<Track> = stems.iter().map(|_| Track::zeros(src.frames, src.channels, None).unwrap()).collect();
    let opts = pipeline::RunOptions {
        overlap: 0.25,
        shifts: 1,
        segment: pipeline::Segment::parse("auto").unwrap(),
        channel_swap: false,
        seed: 0,
    };
    stream::run_member(model.as_ref(), &src, v.sample_rate, &opts, 1 << 18, &mut |_| Ok(()), &mut |at, blk| {
        for (t, b) in outs.iter_mut().zip(blk.iter()) {
            t.write(at, b)?;
        }
        Ok(())
    })
    .unwrap();
    let sr = v.sample_rate;
    let lead = outs[stems.iter().position(|s| s == "lead").unwrap()].to_buffer(sr).unwrap();
    // backing = stem - lead, as the cascade's residual assignment makes it
    let back = AudioBuffer {
        sample_rate: sr,
        data: v.data.iter().zip(&lead.data).map(|(a, b)| a.iter().zip(b).map(|(x, y)| x - y).collect()).collect(),
    };
    (lead, back, Default::default())
}

/// VS_IRM=1: the ideal ratio mask |L|²/(|L|²+|B|²) with the same STFT (an upper bound).
fn irm(v: &AudioBuffer, l: &AudioBuffer, b: &AudioBuffer) -> (AudioBuffer, AudioBuffer, vocal_split::Stats) {
    use ferraus_separate::stft::{istft, stft};
    let n = v.frames();
    let n_fft = vocal_split::n_fft_for(v.sample_rate);
    let mut lead = Vec::new();
    let mut back = Vec::new();
    for c in 0..v.channels() {
        let pad = |x: &[f32]| {
            let mut y = x.to_vec();
            y.resize(n, 0.0);
            y
        };
        let mut sv = stft(&v.data[c], n_fft, n_fft / 4);
        let (sl, sb) = (stft(&pad(&l.data[c]), n_fft, n_fft / 4), stft(&pad(&b.data[c]), n_fft, n_fft / 4));
        for ((fv, fl), fb) in sv.frames.iter_mut().zip(&sl.frames).zip(&sb.frames) {
            for ((x, a), q) in fv.iter_mut().zip(fl).zip(fb) {
                let (pa, pb) = (a.norm_sqr(), q.norm_sqr());
                *x *= if pa + pb > 0.0 { pa / (pa + pb) } else { 0.5 };
            }
        }
        let y = istft(&sv, n);
        back.push(v.data[c].iter().zip(&y).map(|(a, b)| a - b).collect());
        lead.push(y);
    }
    let sr = v.sample_rate;
    (AudioBuffer { sample_rate: sr, data: lead }, AudioBuffer { sample_rate: sr, data: back }, Default::default())
}

/// VS_CENTER=1: the stereo cue alone (lead = centred part, no pitch); mono input passes through.
fn center(v: &AudioBuffer) -> (AudioBuffer, AudioBuffer, vocal_split::Stats) {
    use ferraus_separate::stft::{istft, stft};
    let n = v.frames();
    let sr = v.sample_rate;
    if v.channels() != 2 {
        return (v.clone(), AudioBuffer::new(sr, v.channels(), n), Default::default());
    }
    let n_fft = vocal_split::n_fft_for(sr);
    let mut s: Vec<_> = v.data.iter().map(|c| stft(c, n_fft, n_fft / 4)).collect();
    for f in 0..s[0].frames.len() {
        for k in 0..s[0].frames[f].len() {
            let (l, r) = (s[0].frames[f][k], s[1].frames[f][k]);
            let den = l.norm_sqr() + r.norm_sqr();
            // the method's own centre mapping (Options::stereo_lo/hi/power)
            let o = Options::default();
            let x = (2.0 * (l.re * r.re + l.im * r.im) / den.max(1e-20) - o.stereo_lo as f32)
                / (o.stereo_hi - o.stereo_lo) as f32;
            let c = if den > 1e-20 { x.clamp(0.0, 1.0).powf(o.stereo_power as f32) } else { 1.0 };
            s[0].frames[f][k] *= c;
            s[1].frames[f][k] *= c;
        }
    }
    let lead: Vec<Vec<f32>> = s.iter().map(|x| istft(x, n)).collect();
    let back = v.data.iter().zip(&lead).map(|(a, b)| a.iter().zip(b).map(|(x, y)| x - y).collect()).collect();
    (AudioBuffer { sample_rate: sr, data: lead }, AudioBuffer { sample_rate: sr, data: back }, Default::default())
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() {
        return f64::NAN;
    }
    let m = v.len() / 2;
    if v.len() % 2 == 1 {
        v[m]
    } else {
        (v[m - 1] + v[m]) / 2.0
    }
}

fn main() {
    let o = options();
    let dirs: Vec<String> = std::env::args().skip(1).collect();
    let mut cols: Vec<Vec<f64>> = vec![Vec::new(); 8];
    println!(
        "{:<10} {:>7} {:>7} {:>7} {:>7} | {:>7} {:>7} {:>7} {:>7}  voiced",
        "dir", "base", "lead", "back", "bbase", "m.base", "m.lead", "m.back", "m.bbase"
    );
    for d in &dirs {
        let p = std::path::Path::new(d);
        let (mut v, l, b) = (read(&p.join("vocals.wav")), read(&p.join("lead.wav")), read(&p.join("backing.wav")));
        // VS_CLEAN=1: the stem is the lead alone (how much of the lead the mask keeps)
        if std::env::var("VS_CLEAN").is_ok() {
            v = l.clone();
            v.resize(b.frames().max(l.frames()));
        }
        let n = v.frames();
        let mut row = Vec::new();
        let mut voiced = 0.0;
        for (vv, ll, bb) in [(v.clone(), l.clone(), b.clone()), (mono(&v), mono(&l), mono(&b))] {
            let (el, eb, st) = if std::env::var("VS_IRM").is_ok() {
                irm(&vv, &ll, &bb)
            } else if std::env::var("VS_MODEL").is_ok() {
                model_split(&vv)
            } else if std::env::var("VS_CENTER").is_ok() {
                center(&vv)
            } else {
                run(&vv, &ll, &o)
            };
            voiced = st.voiced_fraction();
            if std::env::var("VS_STATS").is_ok() {
                eprintln!(
                    "{d}: voiced {:.2} conf {:.2} backing {:.1} dB",
                    st.voiced_fraction(),
                    st.mean_confidence,
                    st.backing_ratio_db()
                );
            }
            let (rl, rb, fv) = (flat(&ll, n), flat(&bb, n), flat(&vv, n));
            row.push(si_sdr(&fv, &rl));
            row.push(si_sdr(&flat(&el, n), &rl));
            row.push(si_sdr(&flat(&eb, n), &rb));
            row.push(si_sdr(&fv, &rb));
        }
        let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        println!(
            "{:<10} {:>7.2} {:>7.2} {:>7.2} {:>7.2} | {:>7.2} {:>7.2} {:>7.2} {:>7.2}  {:.2}",
            name, row[0], row[1], row[2], row[3], row[4], row[5], row[6], row[7], voiced
        );
        for (c, x) in cols.iter_mut().zip(row) {
            c.push(x);
        }
    }
    let m: Vec<f64> = cols.into_iter().map(median).collect();
    println!(
        "{:<10} {:>7.2} {:>7.2} {:>7.2} {:>7.2} | {:>7.2} {:>7.2} {:>7.2} {:>7.2}",
        "median", m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7]
    );
}

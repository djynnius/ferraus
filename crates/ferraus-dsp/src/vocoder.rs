//! Channel vocoder (filter bank) and spectral vocoder (STFT cross-synthesis), plus the
//! carrier sources they need: band-limited oscillators (PolyBLEP saw/square/pulse, noise),
//! an autocorrelation pitch tracker for `--follow`, and MIDI note parsing for `--midi`.

use crate::biquad::{octaves_to_q, Biquad32, Cascade, Coeffs, FilterType};
use crate::dynamics::EnvelopeFollower;
use crate::stft::Stft;
use crate::util::{rms, Noise};
use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

/// Built-in carrier waveforms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waveform {
    Saw,
    Square,
    /// 25% duty pulse.
    Pulse,
    Noise,
}

impl Waveform {
    pub fn parse(s: &str) -> Option<Waveform> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "saw" | "sawtooth" => Waveform::Saw,
            "square" => Waveform::Square,
            "pulse" => Waveform::Pulse,
            "noise" => Waveform::Noise,
            _ => return None,
        })
    }
}

#[inline]
fn poly_blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

/// Band-limited oscillator (PolyBLEP).
#[derive(Clone, Debug)]
pub struct Oscillator {
    pub wave: Waveform,
    phase: f64,
    noise: Noise,
}

impl Oscillator {
    pub fn new(wave: Waveform, seed: u64) -> Oscillator {
        Oscillator { wave, phase: 0.0, noise: Noise::new(seed) }
    }

    /// Next sample at `freq` Hz.
    #[inline]
    pub fn next(&mut self, freq: f64, sample_rate: f64) -> f32 {
        if self.wave == Waveform::Noise {
            return self.noise.tick() * 0.577;
        }
        let dt = (freq / sample_rate).clamp(0.0, 0.49);
        let t = self.phase;
        let y = match self.wave {
            Waveform::Saw => 2.0 * t - 1.0 - poly_blep(t, dt),
            Waveform::Square | Waveform::Pulse => {
                let duty = if self.wave == Waveform::Square { 0.5 } else { 0.25 };
                let naive = if t < duty { 1.0 } else { -1.0 };
                naive + poly_blep(t, dt) - poly_blep((t + 1.0 - duty) % 1.0, dt)
            }
            Waveform::Noise => 0.0,
        };
        self.phase += dt;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        (y * 0.5) as f32
    }
}

/// Render an oscillator following a per-sample frequency track (`None`/0 = silence).
pub fn render_osc(wave: Waveform, freqs: &[f64], sample_rate: f64, seed: u64) -> Vec<f32> {
    let mut o = Oscillator::new(wave, seed);
    freqs.iter().map(|f| if *f > 0.0 { o.next(*f, sample_rate) } else { 0.0 }).collect()
}

/// Autocorrelation pitch tracker. Returns one estimate per `hop` samples (frame centres at
/// `k·hop`), `None` where the frame is unvoiced or silent.
pub fn track_pitch(x: &[f32], sample_rate: f64, hop: usize, fmin: f64, fmax: f64) -> Vec<Option<f64>> {
    let win = ((sample_rate / fmin) * 2.5).ceil() as usize;
    let win = win.next_power_of_two().max(512);
    let nfft = win * 2;
    let mut planner = RealFftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(nfft);
    let inv = planner.plan_fft_inverse(nfft);
    let lag_min = (sample_rate / fmax).floor().max(2.0) as usize;
    let lag_max = ((sample_rate / fmin).ceil() as usize).min(win - 1);
    let mut buf = vec![0.0f32; nfft];
    let mut spec = fwd.make_output_vec();
    let mut ac = vec![0.0f32; nfft];
    let mut out = Vec::new();
    let mut centre = 0usize;
    let global = rms(x).max(1e-9);
    while centre < x.len() + hop {
        let start = centre as isize - win as isize / 2;
        let mut energy = 0.0f32;
        for (i, b) in buf.iter_mut().enumerate() {
            *b = if i < win {
                let idx = start + i as isize;
                if idx >= 0 && (idx as usize) < x.len() {
                    x[idx as usize]
                } else {
                    0.0
                }
            } else {
                0.0
            };
            energy += *b * *b;
        }
        let frame_rms = (energy / win as f32).sqrt();
        if frame_rms < global * 0.03 || frame_rms < 1e-5 {
            out.push(None);
            centre += hop;
            continue;
        }
        fwd.process(&mut buf, &mut spec).expect("fft sizes match");
        for c in spec.iter_mut() {
            *c = Complex::new(c.norm_sqr(), 0.0);
        }
        inv.process(&mut spec, &mut ac).expect("fft sizes match");
        let r0 = ac[0].max(1e-12);
        // normalise by the overlap length to undo the triangular bias
        let norm = |l: usize| ac[l] / r0 * win as f32 / (win - l) as f32;
        let mut best = 0.0f32;
        for l in lag_min..=lag_max {
            best = best.max(norm(l));
        }
        let mut pick = None;
        if best > 0.45 {
            // first local maximum close to the best (avoids octave-down errors)
            for l in lag_min + 1..lag_max {
                let v = norm(l);
                if v >= 0.9 * best && v >= norm(l - 1) && v >= norm(l + 1) {
                    let (a, b, c) = (norm(l - 1), v, norm(l + 1));
                    let den = a - 2.0 * b + c;
                    let off = if den.abs() > 1e-12 { 0.5 * (a - c) / den } else { 0.0 };
                    pick = Some(sample_rate / (l as f64 + off as f64));
                    break;
                }
            }
        }
        out.push(pick);
        centre += hop;
    }
    out
}

/// Expand frame estimates into a per-sample frequency track, holding the last voiced pitch
/// (or `default`) through unvoiced frames and gliding between frames.
pub fn pitch_track_to_samples(frames: &[Option<f64>], hop: usize, len: usize, default: f64) -> Vec<f64> {
    let mut held = Vec::with_capacity(frames.len());
    let mut last = frames.iter().flatten().next().copied().unwrap_or(default);
    for f in frames {
        if let Some(v) = f {
            last = *v;
        }
        held.push(last);
    }
    (0..len)
        .map(|i| {
            let k = i / hop;
            let t = (i % hop) as f64 / hop as f64;
            let a = held.get(k).copied().unwrap_or(last);
            let b = held.get(k + 1).copied().unwrap_or(a);
            a * (b / a).powf(t)
        })
        .collect()
}

/// Renders a plugin instrument as a vocoder carrier: (plugin name or path, notes,
/// length in frames, sample rate) → mono samples. Registered by the application (the plugin
/// host lives in another crate).
pub type PluginCarrier = fn(&str, &[MidiNote], usize, u32) -> ferraus_core::Result<Vec<f32>>;

static PLUGIN_CARRIER: std::sync::OnceLock<PluginCarrier> = std::sync::OnceLock::new();

/// Register the plugin-carrier renderer (once per process).
pub fn set_plugin_carrier(f: PluginCarrier) -> bool {
    PLUGIN_CARRIER.set(f).is_ok()
}

/// The registered plugin-carrier renderer, if any.
pub fn plugin_carrier() -> Option<PluginCarrier> {
    PLUGIN_CARRIER.get().copied()
}

/// A note parsed from a MIDI file.
#[derive(Clone, Debug, PartialEq)]
pub struct MidiNote {
    pub start_s: f64,
    pub end_s: f64,
    pub key: u8,
    pub velocity: u8,
}

/// Parse note on/off events from a Standard MIDI File, honouring the tempo map.
pub fn parse_midi_notes(bytes: &[u8]) -> Result<Vec<MidiNote>, String> {
    use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
    let smf = Smf::parse(bytes).map_err(|e| format!("cannot parse MIDI file: {e}"))?;
    enum Ev {
        On(u8, u8, u8),
        Off(u8, u8),
        Tempo(u32),
    }
    let mut events: Vec<(u64, usize, Ev)> = Vec::new();
    let mut seq = 0usize;
    for track in &smf.tracks {
        let mut t = 0u64;
        for ev in track {
            t += ev.delta.as_int() as u64;
            let e = match ev.kind {
                TrackEventKind::Midi { channel, message } => match message {
                    MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                        Some(Ev::On(channel.as_int(), key.as_int(), vel.as_int()))
                    }
                    MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                        Some(Ev::Off(channel.as_int(), key.as_int()))
                    }
                    _ => None,
                },
                TrackEventKind::Meta(MetaMessage::Tempo(us)) => Some(Ev::Tempo(us.as_int())),
                _ => None,
            };
            if let Some(e) = e {
                events.push((t, seq, e));
                seq += 1;
            }
        }
    }
    events.sort_by_key(|e| (e.0, e.1));
    let (tick_s_fixed, tpb) = match smf.header.timing {
        Timing::Metrical(t) => (None, t.as_int().max(1) as f64),
        Timing::Timecode(fps, sub) => (Some(1.0 / (fps.as_f32() as f64 * sub.max(1) as f64)), 1.0),
    };
    let mut tempo = 500_000.0f64;
    let mut last_tick = 0u64;
    let mut now = 0.0f64;
    let mut open: Vec<(u8, u8, u8, f64)> = Vec::new();
    let mut notes = Vec::new();
    for (tick, _, e) in events {
        let dt = (tick - last_tick) as f64;
        now += match tick_s_fixed {
            Some(s) => dt * s,
            None => dt * tempo / 1e6 / tpb,
        };
        last_tick = tick;
        match e {
            Ev::Tempo(us) => tempo = us.max(1) as f64,
            Ev::On(ch, key, vel) => open.push((ch, key, vel, now)),
            Ev::Off(ch, key) => {
                if let Some(pos) = open.iter().position(|o| o.0 == ch && o.1 == key) {
                    let o = open.remove(pos);
                    notes.push(MidiNote { start_s: o.3, end_s: now, key, velocity: o.2 });
                }
            }
        }
    }
    for o in open {
        notes.push(MidiNote { start_s: o.3, end_s: now, key: o.1, velocity: o.2 });
    }
    notes.sort_by(|a, b| a.start_s.partial_cmp(&b.start_s).unwrap_or(std::cmp::Ordering::Equal));
    Ok(notes)
}

/// Render MIDI notes with an oscillator per note (polyphonic, 5 ms attack/release ramps).
pub fn render_midi(wave: Waveform, notes: &[MidiNote], len: usize, sample_rate: f64, seed: u64) -> Vec<f32> {
    let mut out = vec![0.0f32; len];
    let ramp = (0.005 * sample_rate).max(1.0);
    for (i, n) in notes.iter().enumerate() {
        let s = (n.start_s * sample_rate).round() as usize;
        let e = ((n.end_s * sample_rate).round() as usize).min(len);
        if s >= len || e <= s {
            continue;
        }
        let f = ferraus_core::units::midi_to_hz(n.key as f64);
        let mut o = Oscillator::new(wave, seed.wrapping_add(i as u64));
        let vel = 0.4 + 0.6 * n.velocity as f32 / 127.0;
        let tail = (ramp as usize).min(len - e);
        for (k, v) in out[s..e + tail].iter_mut().enumerate() {
            let t = k as f64;
            let env = (t / ramp).min(1.0).min(((e - s) as f64 + ramp - t) / ramp).clamp(0.0, 1.0);
            *v += o.next(f, sample_rate) * vel * env as f32;
        }
    }
    out
}

/// Vocoder settings shared by both modes.
#[derive(Clone, Debug)]
pub struct VocoderParams {
    pub bands: usize,
    pub lo: f64,
    pub hi: f64,
    pub attack_s: f64,
    pub release_s: f64,
    /// Formant shift in semitones (positive = brighter / smaller).
    pub formant_shift: f64,
    /// Resynthesize sibilants from noise.
    pub unvoiced: bool,
    pub seed: u64,
}

impl Default for VocoderParams {
    fn default() -> Self {
        VocoderParams {
            bands: 24,
            lo: 80.0,
            hi: 8000.0,
            attack_s: 0.005,
            release_s: 0.030,
            formant_shift: 0.0,
            unvoiced: true,
            seed: 0,
        }
    }
}

/// Log-spaced band centres.
pub fn band_centres(bands: usize, lo: f64, hi: f64) -> Vec<f64> {
    let n = bands.max(2);
    (0..n).map(|i| lo * (hi / lo).powf(i as f64 / (n - 1) as f64)).collect()
}

/// Sibilance amount 0..1 from the ratio of high-band to full-band envelope.
#[inline]
fn sibilance(hf_env: f32, all_env: f32) -> f32 {
    let r = hf_env / (all_env + 1e-9);
    ((r - 0.3) / 0.3).clamp(0.0, 1.0)
}

/// Scale `y` so its short-term level inside `lo..hi` follows `reference`'s level in the same
/// band (30 ms zero-phase power envelopes, gain capped at +24 dB). Energy that `y` adds
/// above the band, such as the sibilance "air" band, does not affect the gain.
fn match_rms(y: &mut [f32], reference: &[f32], lo: f64, hi: f64, sample_rate: f64) {
    let c = crate::util::time_coeff(0.030, sample_rate);
    let env = |x: &[f32]| -> Vec<f32> {
        let mut b = x.to_vec();
        Cascade::butterworth_highpass(2, lo, sample_rate).process_block(&mut b);
        Cascade::butterworth_lowpass(4, hi, sample_rate).process_block(&mut b);
        let mut e = vec![0.0f32; b.len()];
        let mut s = 0.0f32;
        for (o, v) in e.iter_mut().zip(&b) {
            s = crate::util::flush(v * v + c * (s - v * v));
            *o = s;
        }
        let mut s = 0.0f32;
        for o in e.iter_mut().rev() {
            s = crate::util::flush(*o + c * (s - *o));
            *o = s;
        }
        e
    };
    let ey = env(y);
    let er = env(reference);
    let floor = ey.iter().cloned().fold(0.0f32, f32::max) * 1e-8 + 1e-14;
    for ((v, a), b) in y.iter_mut().zip(&ey).zip(&er) {
        let g = if *a > floor { (b / a).sqrt().min(16.0) } else { 0.0 };
        *v *= g;
    }
}

/// Bands processed together in one pass (independent filters pipeline and vectorise well).
const LANES: usize = 8;

/// Up to [`LANES`] 4th-order band-passes (two identical constant-peak sections each) run in
/// lockstep on the same input. Unused lanes have zero coefficients and output silence.
struct BandBank {
    c: [[f32; 5]; LANES],
    s: [[f32; 4]; LANES],
}

impl BandBank {
    fn new(centres: &[f64], shift: f64, q: f64, sample_rate: f64) -> BandBank {
        let mut c = [[0.0f32; 5]; LANES];
        for (k, f) in centres.iter().enumerate().take(LANES) {
            let d = Coeffs::design(FilterType::Bandpass, f * shift, q, 0.0, sample_rate);
            c[k] = [d.b0 as f32, d.b1 as f32, d.b2 as f32, d.a1 as f32, d.a2 as f32];
        }
        BandBank { c, s: [[0.0; 4]; LANES] }
    }

    #[inline(always)]
    fn process(&mut self, x: f32) -> [f32; LANES] {
        let mut out = [0.0f32; LANES];
        for j in 0..LANES {
            let [b0, b1, b2, a1, a2] = self.c[j];
            let st = &mut self.s[j];
            let y = b0 * x + st[0];
            st[0] = b1 * x - a1 * y + st[1];
            st[1] = b2 * x - a2 * y;
            let y2 = b0 * y + st[2];
            st[2] = b1 * y - a1 * y2 + st[3];
            st[3] = b2 * y - a2 * y2;
            out[j] = y2;
        }
        out
    }

    /// Denormal protection, called every few samples (state cannot decay from 1e-20 to the
    /// subnormal range within that time).
    fn flush(&mut self) {
        for st in &mut self.s {
            for v in st.iter_mut() {
                *v = crate::util::flush(*v);
            }
        }
    }
}

/// Classic channel vocoder: band-pass filter banks on modulator and carrier, envelope
/// followers on the modulator bands, per-band carrier normalisation, and optional noise
/// resynthesis of sibilants (the carrier input crossfades to white noise while the
/// modulator's high-band energy ratio says "sibilant", and an "air" band of noise above the
/// range follows the modulator's own highs). Output level follows the modulator.
///
/// Processes band by band over the whole signal with `f32` filters (cache friendly).
pub fn vocode_classic(modulator: &[f32], carrier: &[f32], sample_rate: f64, p: &VocoderParams) -> Vec<f32> {
    let n = modulator.len();
    let centres = band_centres(p.bands, p.lo, p.hi.min(sample_rate * 0.45));
    let nb = centres.len();
    let ratio = (centres[nb - 1] / centres[0]).powf(1.0 / (nb - 1) as f64);
    let q = octaves_to_q(ratio.log2()) * 1.2;
    let shift = 2f64.powf(-p.formant_shift / 12.0);
    let mut noise = Noise::new(p.seed ^ 0x5eed);
    let mut car: Vec<f32> = (0..n).map(|i| carrier.get(i).copied().unwrap_or(0.0)).collect();
    // sibilance amount per sample, and the carrier crossfade to noise
    let sib: Vec<f32> = if p.unvoiced {
        let mut hf_det = Biquad32::new(Coeffs::design(FilterType::Highpass, 4000.0, 0.707, 0.0, sample_rate));
        let mut hf_env = EnvelopeFollower::new(p.attack_s, p.release_s, sample_rate);
        let mut all_env = EnvelopeFollower::new(p.attack_s, p.release_s, sample_rate);
        let s: Vec<f32> = modulator
            .iter()
            .map(|m| {
                let h = hf_env.process(hf_det.process(*m).abs());
                let a = all_env.process(m.abs());
                sibilance(h, a)
            })
            .collect();
        let crms = rms(&car).max(1e-9);
        for (c, sv) in car.iter_mut().zip(&s) {
            *c = (1.0 - sv) * *c / crms + sv * noise.tick() * 1.732;
        }
        s
    } else {
        Vec::new()
    };
    let floor = (rms(&car) * 1e-3).max(1e-7);
    let mut out = vec![0.0f32; n];
    let att = crate::util::time_coeff(p.attack_s, sample_rate);
    let rel = crate::util::time_coeff(p.release_s, sample_rate);
    for group in centres.chunks(LANES) {
        let mut mod_bank = BandBank::new(group, shift, q, sample_rate);
        let mut car_bank = BandBank::new(group, 1.0, q, sample_rate);
        let mut em = [0.0f32; LANES];
        let mut ec = [0.0f32; LANES];
        for (i, (o, (m, c))) in out.iter_mut().zip(modulator.iter().zip(&car)).enumerate() {
            let ym = mod_bank.process(*m);
            let yc = car_bank.process(*c);
            let mut acc = 0.0f32;
            for j in 0..LANES {
                let a = ym[j].abs();
                em[j] = a + if a > em[j] { att } else { rel } * (em[j] - a);
                let b = yc[j].abs();
                ec[j] = b + if b > ec[j] { att } else { rel } * (ec[j] - b);
                acc += em[j] * yc[j] / ec[j].max(floor);
            }
            *o += acc;
            if i % 64 == 63 {
                mod_bank.flush();
                car_bank.flush();
                for v in em.iter_mut().chain(ec.iter_mut()) {
                    *v = crate::util::flush(*v);
                }
            }
        }
    }
    if p.unvoiced {
        let air_fc = p.hi.min(sample_rate * 0.4);
        let mut air_mod = Cascade::butterworth_highpass(4, air_fc, sample_rate);
        let mut air_noise = Cascade::butterworth_highpass(4, air_fc, sample_rate);
        let mut air_env = EnvelopeFollower::new(p.attack_s, p.release_s, sample_rate);
        for ((o, m), sv) in out.iter_mut().zip(modulator).zip(&sib) {
            let ea = air_env.process(air_mod.process(*m).abs());
            let an = air_noise.process(noise.tick());
            *o += sv * ea * an * 2.0;
        }
    }
    match_rms(&mut out, modulator, centres[0], centres[nb - 1], sample_rate);
    out
}

/// Spectral vocoder: STFT cross-synthesis. The carrier spectrum is whitened by its own
/// band envelope and re-shaped by the modulator's (optionally formant-shifted) envelope.
pub fn vocode_spectral(modulator: &[f32], carrier: &[f32], sample_rate: f64, p: &VocoderParams) -> Vec<f32> {
    let n = modulator.len();
    let size = if sample_rate > 64000.0 { 4096 } else { 2048 };
    let hop = size / 4;
    let stft = Stft::new(size, hop);
    let mut car: Vec<f32> = carrier.to_vec();
    car.resize(n, 0.0);
    let mf = stft.analyze(modulator);
    let mut cf = stft.analyze(&car);
    let bins = stft.bins();
    let df = sample_rate / size as f64;
    let hi = p.hi.min(sample_rate * 0.45);
    let centres = band_centres(p.bands, p.lo, hi);
    let nb = centres.len();
    // band edges halfway (geometric) between centres
    let edge = |k: usize| -> f64 {
        if k == 0 {
            centres[0] / (centres[1] / centres[0]).sqrt()
        } else if k == nb {
            centres[nb - 1] * (centres[nb - 1] / centres[nb - 2]).sqrt()
        } else {
            (centres[k - 1] * centres[k]).sqrt()
        }
    };
    let band_bins: Vec<(usize, usize)> = (0..nb)
        .map(|b| {
            let lo = (edge(b) / df).floor().max(1.0) as usize;
            let hi = ((edge(b + 1) / df).ceil() as usize).clamp(lo + 1, bins - 1);
            (lo, hi)
        })
        .collect();
    let band_env = |frame: &[Complex<f32>]| -> Vec<f32> {
        band_bins
            .iter()
            .map(|&(a, b)| {
                let e: f32 = frame[a..b].iter().map(|c| c.norm_sqr()).sum();
                (e / (b - a) as f32).sqrt()
            })
            .collect()
    };
    // per-bin interpolation of a band envelope (linear in log frequency)
    let lc: Vec<f64> = centres.iter().map(|c| c.ln()).collect();
    let interp = |env: &[f32], f: f64| -> f32 {
        if f <= centres[0] {
            return env[0];
        }
        if f >= centres[nb - 1] {
            return env[nb - 1];
        }
        let lf = f.ln();
        let k = lc.partition_point(|c| *c <= lf).clamp(1, nb - 1);
        let t = ((lf - lc[k - 1]) / (lc[k] - lc[k - 1])) as f32;
        env[k - 1] + t * (env[k] - env[k - 1])
    };
    let frame_s = hop as f64 / sample_rate;
    let att = crate::util::time_coeff(p.attack_s.max(frame_s * 0.5), 1.0 / frame_s);
    let rel = crate::util::time_coeff(p.release_s.max(frame_s * 0.5), 1.0 / frame_s);
    let mut sm = vec![0.0f32; nb];
    let shift = 2f64.powf(-p.formant_shift / 12.0);
    let mut noise = Noise::new(p.seed ^ 0xabcdef);
    let glob = cf.iter().flat_map(|f| f.iter()).map(|c| c.norm()).fold(0.0f32, f32::max) * 1e-4 + 1e-9;
    let lo_bin = (edge(0) / df).floor() as usize;
    let hi_bin = (edge(nb) / df).ceil() as usize;
    let hf_bin = (4000.0 / df) as usize;
    for (mfr, cfr) in mf.iter().zip(cf.iter_mut()) {
        let em = band_env(mfr);
        for (s, e) in sm.iter_mut().zip(&em) {
            let c = if *e > *s { att } else { rel };
            *s = e + c * (*s - e);
        }
        let ec = band_env(cfr);
        let sib = if p.unvoiced {
            let tot: f32 = mfr.iter().map(|c| c.norm_sqr()).sum::<f32>() + 1e-12;
            let hf: f32 = mfr[hf_bin.min(bins - 1)..].iter().map(|c| c.norm_sqr()).sum();
            sibilance((hf / tot).sqrt(), 1.0)
        } else {
            0.0
        };
        for (k, c) in cfr.iter_mut().enumerate() {
            let f = k as f64 * df;
            if k < lo_bin {
                *c = Complex::new(0.0, 0.0);
                continue;
            }
            if k > hi_bin {
                // air band: noise shaped by the modulator's own magnitude, only for sibilants
                if p.unvoiced && sib > 0.0 {
                    let mag = mfr[k].norm() * sib;
                    let ph = noise.tick() * std::f32::consts::PI;
                    *c = Complex::from_polar(mag, ph);
                } else {
                    *c = Complex::new(0.0, 0.0);
                }
                continue;
            }
            let target = interp(&sm, f * shift);
            let cen = interp(&ec, f).max(glob);
            let mut v = *c / cen;
            if sib > 0.0 {
                let ph = noise.tick() * std::f32::consts::PI;
                v = v * (1.0 - sib) + Complex::from_polar(sib, ph);
            }
            *c = v * target;
        }
    }
    let mut out = stft.synthesize(&mut cf, n);
    match_rms(&mut out, modulator, centres[0], centres[nb - 1], sample_rate);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_tracker_finds_a_sine() {
        let sr = 16000.0;
        let x: Vec<f32> =
            (0..16000).map(|i| (2.0 * std::f64::consts::PI * 220.0 * i as f64 / sr).sin() as f32 * 0.5).collect();
        let frames = track_pitch(&x, sr, 256, 60.0, 1000.0);
        let voiced: Vec<f64> = frames[10..frames.len() - 10].iter().flatten().copied().collect();
        assert!(voiced.len() > 40);
        for f in voiced {
            assert!((f - 220.0).abs() < 3.0, "{f}");
        }
    }

    #[test]
    fn oscillator_has_expected_period() {
        let sr = 48000.0;
        let mut o = Oscillator::new(Waveform::Saw, 1);
        let y: Vec<f32> = (0..48000).map(|_| o.next(100.0, sr)).collect();
        let resets = y.windows(2).filter(|w| w[0] >= 0.0 && w[1] < 0.0 && w[0] - w[1] > 0.1).count();
        assert!((99..=101).contains(&resets), "{resets}");
    }
}

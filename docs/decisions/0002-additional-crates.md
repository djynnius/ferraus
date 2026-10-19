# ADR 0002: Crates beyond the PRD list

Status: accepted (2026-10-19)

The PRD's crate table is the minimum. To let milestones be built in parallel with clear
ownership, four crates were added:

| Crate | Holds | PRD home |
| --- | --- | --- |
| `ferraus-edit` | trim, cut, split, concat, resample, gain, normalize, channels, silence, mix, waveform, spectrogram | "core audio operations" (was implicitly in dsp/cli) |
| `ferraus-restore` | Audition-parity restoration and utility filters | "restoration, panning, and Audition-parity filters" |
| `ferraus-analysis` | tempo, loudness, spectrum, chords, problems, stats, compare | "analysis and pipelines" |
| `ferraus-master` | master, master analyze/plan/verify/deliver | "mastering" |

Composite ops (enhance, master, hum) call other ops by name at run time through the registry
(`ferraus_core::exec::run_transform`), so these crates do not depend on each other at compile time.

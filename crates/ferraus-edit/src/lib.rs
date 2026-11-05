//! # ferraus-edit
//!
//! The editing commands: `trim`, `cut`, `split`, `concat`, `resample`, `gain`, `normalize`,
//! `channels`, `silence detect|trim|pad`, `mix`, `mix balance`, `waveform`, and `spectrogram`.
//!
//! Besides the ops, the crate exports reusable building blocks:
//!
//! * [`loudness`] — EBU R128 integrated loudness, a 4x oversampled true-peak meter, and a
//!   transparent lookahead true-peak limiter.
//! * [`silence::detect_silence`] — the silence map used by `split`, `silence trim`, and
//!   `silence detect`.
//! * [`util::snap_to_zero`], [`util::remove_ranges`], [`range_ops::concat_buffers`],
//!   [`mix::mix_buffers`], [`mix::pan_gains`], [`visual::stft_power`],
//!   [`visual::mel_filterbank`].
//! * [`balance::canonical_role`], [`balance::role_from_name`], [`balance::role_offset`] — the
//!   stem roles and genre balance template behind `mix balance`.

use ferraus_core::OpSpec;

pub mod balance;
pub mod channels;
pub mod level;
pub mod loudness;
pub mod mix;
pub mod range_ops;
pub mod silence;
pub mod util;
pub mod visual;

#[cfg(test)]
mod tests;

/// Ops this crate contributes to the command registry.
pub fn ops() -> Vec<OpSpec> {
    let mut v = Vec::new();
    v.extend(range_ops::ops());
    v.extend(level::ops());
    v.extend(channels::ops());
    v.extend(silence::ops());
    v.extend(mix::ops());
    v.extend(balance::ops());
    v.extend(visual::ops());
    v
}

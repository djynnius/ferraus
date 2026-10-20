# ADR 0008: License review decisions (2026-10-20)

Status: accepted (2026-10-20), by the maintainer, on the questions in
[`docs/legal/LICENSE-REVIEW.md`](../legal/LICENSE-REVIEW.md). These are the maintainer's
decisions, not legal advice; the review packet stays available for counsel.

## 1. Mel-Band RoFormer weights (`separate/mel-roformer-kj`): MIT accepted

The author's (Kimberley Jensen) weights repository states MIT, and the hosted ONNX export
carries the same license. An unrelated OpenVINO conversion labels the model GPL-3.0; a third
party's label on its own conversion does not change the author's license. Undisclosed
training data is common to the community separation models and is accepted here as it is for
the Demucs models.

Consequence: the registry's `auto_skip` on the `quality` tier is removed, so `separate --tier
auto` (the default) resolves to `quality` as the PRD intends (ADR 0007). The `auto_skip`
mechanism stays for future holds.

## 2. VST3 host (`ferraus-plughost-vst3`): Apache-2.0 OR MIT

The PRD isolated VST3 hosting in a GPLv3 executable because the VST3 SDK was dual-licensed
GPLv3 or proprietary. Steinberg released the VST 3.8 SDK under the MIT license on
2025-10-29. The host uses the `vst3` crate (0.3), whose bindings are generated from the 3.8
headers and licensed MIT OR Apache-2.0, so no GPL code remains in it.

Consequence: the host is licensed Apache-2.0 OR MIT like every other crate, and `deny.toml`
no longer allows GPL for it. It stays a separate executable: process isolation is also what
keeps a crashing plugin from taking ferraus down.

## 3. Verovio engraving (`ferraus-engrave`): unchanged

Verovio (LGPL-3.0) is statically linked only into the separate `ferraus-engrave` executable,
which is itself LGPL-3.0 with public source, so users can rebuild it against a modified
Verovio. The `ferraus` binary contains no Verovio code.

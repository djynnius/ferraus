# License review packet (prepared for counsel; not legal advice)

This document collects the facts counsel needs for the PRD's legal gates: G3 (model license
audit) and G4 (VST3 host licensing). It was prepared by the engineering side and should be
reviewed by a qualified lawyer before 1.0.

## Project license

All crates and the `ferraus` binary: **Apache-2.0 OR MIT**. The binary statically links 218
third-party crates, all under permissive licenses (MIT, Apache-2.0, BSD, ISC, Zlib,
Unicode, CDLA-Permissive-2.0, Unlicense) plus **MPL-2.0** (Symphonia and others: file-level
copyleft; source of those files is available unmodified from crates.io). Full list:
`crates/ferraus-cli/THIRD-PARTY-LICENSES.md` (regenerated with full texts by cargo-about at
release). `deny.toml` enforces this policy in CI.

## Components shipped as separate executables

| Executable | License | Why separate | Talks to ferraus via |
| --- | --- | --- | --- |
| `ferraus-plughost-vst3` | GPL-3.0-only | links VST3 host code; Steinberg's SDK is GPLv3 or proprietary. (Note: it currently uses the `vst3` crate bindings, which are MIT/Apache; counsel should confirm whether GPL is still required or whether the permissive bindings plus Steinberg's interface definitions change that.) | length-prefixed JSON on pipes + a memory-mapped audio file (`crates/ferraus-plugins/PROTOCOL.md`) |
| `ferraus-engrave` | LGPL-3.0-or-later | statically links Verovio (LGPL-3.0); fonts under OFL-1.1 | MusicXML on stdin, SVG/PDF/PNG on stdout |
| `ferraus-plughost` | Apache-2.0 OR MIT | CLAP host (CLAP ABI is MIT) | as above |

Each is released as its own app with its own archive and installer (cargo-dist), so the
permissive `ferraus` archive contains no GPL/LGPL code. Questions for counsel: is process
separation with this protocol sufficient for "aggregate" treatment; are the release
artifacts' license notices adequate.

## Run-time loaded or downloaded components (never linked or bundled)

| Component | License | How it is obtained |
| --- | --- | --- |
| FFmpeg | LGPL or GPL per build | user's PATH, or `doctor --install-ffmpeg` (pinned static build; license shown) |
| libmp3lame | LGPL-2.1 | `dlopen` at run time if installed |
| libopus | BSD-3-Clause | `dlopen` at run time if installed |
| ONNX Runtime GPU providers, CUDA/cuDNN | MIT / NVIDIA EULA | `accelerate install` (EULA shown, `--accept-eula` required) |
| Audiveris | AGPL-3.0 | `doctor --install-audiveris` (separate Java process) |

## Model weights (gate G3)

Registry: `crates/ferraus-models/registry/registry.json`. Only entries with a recorded
permissive license and `commercial_use: true` are eligible for default tiers; entries with no
clear license are flagged `no_clear_license` and need `--model <name> --accept-license`.
Research-only entries (e.g. RMVPE) are flagged and excluded.

| Entry | License as recorded | Used by default |
| --- | --- | --- |
| separate/htdemucs, htdemucs-ft-{drums,bass,other,vocals}, htdemucs-6s | MIT (Meta Demucs v4; ONNX exports by StemSplitio on Hugging Face, MIT cards) | yes (`fast`, `balanced`, `quality`, `max`, `--stems 6`) |
| separate/mel-roformer-kj@2024.08 | MIT per the author's (Kimberley Jensen) weights repository and the export's LICENSE file; an unrelated OpenVINO conversion labels it GPL-3.0; training data undisclosed | `quality`, `max` (`auto` picks `balanced` until this review clears it: the registry's `auto_skip` field on the `quality` tier) |
| separate/drumsep-inagoy@2022 | MIT (inagoy/drumsep; ONNX export by gridshiftstudio) | `--drum-split` |
| denoise/dfn3 | MIT OR Apache-2.0 (DeepFilterNet) | yes |
| denoise/rnnoise | BSD-3-Clause | yes |
| pitch/crepe, crepe-tiny | MIT | on request |
| pitch/swiftf0 | MIT | on request |
| transcribe/basic-pitch | Apache-2.0 (Spotify) | poly mode |
| omr/oemer, omr/oemer-seg | MIT (BreezeWhite/oemer `checkpoints` release) | builtin OMR (`score read`) |
| separate/melody-mask@1 (lead/backing split) | built into ferraus (Apache-2.0 OR MIT), no weights | `--vocal-split` |
| separate/mdx-kara-2 (UVR karaoke, lead/backing split; downloads verified by SHA-256) | none stated on the weights | opt-in only: `--cascade-models vocal-split=separate/mdx-kara-2 --accept-license` (acceptance remembered per model), then a `model_license_unclear` warning on every use |
| Other RoFormer/SCNet community checkpoints, RoFormer karaoke checkpoints | unclear / none (anvuew's BS-RoFormer karaoke: GPL-3.0) | excluded |

Questions for counsel: whether model weights trained on datasets with research-only terms
(e.g. MUSDB18-HQ) can be redistributed under the code license stated by their authors; whether
the Mel-Band RoFormer's MIT statement holds given the GPL-3.0 label on another conversion and
its undisclosed training data (if it does, delete the `auto_skip` field in the registry and
`auto` picks `quality` again; if not, also set the entry's status to `unverified`); and whether downloading from third-party Hugging Face repositories
(pinned by commit and SHA-256) is acceptable or the files should be mirrored to a
`djynnius/ferraus-models` release first.

## Sounds registry

`crates/ferraus-plugins/sounds/registry.json`: GPL-3.0 instruments (Surge XT, Dexed, …),
CC0/CC-BY sample libraries, MIT/free SoundFonts. ferraus downloads them on request, shows the
license first, never runs vendor installers, and installs per user. Attribution requirements
are recorded per entry.

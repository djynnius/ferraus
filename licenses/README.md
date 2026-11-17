# Upstream license texts

License files of C and C++ libraries that ferraus links but cargo cannot list, and texts the
release archives need. `tools/gen-licenses.sh` appends them to the THIRD-PARTY-LICENSES.md files.

| File | Source |
|---|---|
| `onnxruntime/LICENSE`, `onnxruntime/ThirdPartyNotices.txt` | https://github.com/microsoft/onnxruntime at tag v1.31.0 (the version `.github/actions/onnxruntime` builds and `ort` links) |
| `musl/COPYRIGHT` | https://git.musl-libc.org/cgit/musl/plain/COPYRIGHT?h=v1.2.5 |
| `fonts/OFL-1.1.txt` | https://openfontlicense.org/documents/OFL.txt (the SMuFL fonts embedded in ferraus-engrave) |
| `basic-pitch/NOTICE` | https://github.com/spotify/basic-pitch (copied into the root NOTICE) |

Update them when the ONNX Runtime version changes, then run `tools/gen-licenses.sh`.

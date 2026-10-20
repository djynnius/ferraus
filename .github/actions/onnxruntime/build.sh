#!/usr/bin/env bash
# Builds ONNX Runtime from source as static libraries for a target that ort (the Rust crate) has
# no prebuilt library for, and copies the libraries to OUT_DIR in the layout ort-sys's
# ORT_LIB_LOCATION static-link search expects (the CMake binary directory: libonnxruntime_*.a at
# the top, dependencies under _deps/).
#
#   build.sh <target-triple> <onnxruntime-version> <out-dir>
#
# Runs on the macOS Intel runner for x86_64-apple-darwin and inside an Alpine container for the
# *-unknown-linux-musl targets (see action.yml). Called only from CI.
set -euo pipefail

target="$1"
version="$2"
out="$3"

# The version must match the ONNX Runtime API that the pinned ort crate binds (ort 2.0.0-rc.14:
# ONNX Runtime 1.31). The archive checksum pins the source.
case "$version" in
  1.31.0) sha256="a7e84230b5d509e48fbd6b73c2453a56345ccb22da34a46dac1f0cb4a2d6507b" ;;
  *) echo "no pinned source checksum for ONNX Runtime $version; add it to build.sh" >&2; exit 2 ;;
esac

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
archive="$work/onnxruntime-$version.tar.gz"
curl -fsSL --retry 3 -o "$archive" "https://github.com/microsoft/onnxruntime/archive/refs/tags/v$version.tar.gz"
if command -v sha256sum >/dev/null 2>&1; then
  echo "$sha256  $archive" | sha256sum -c -
else
  echo "$sha256  $archive" | shasum -a 256 -c -
fi
tar -xzf "$archive" -C "$work"
src="$work/onnxruntime-$version"

defines=(
  CMAKE_POSITION_INDEPENDENT_CODE=ON
  CMAKE_POLICY_VERSION_MINIMUM=3.5
  # --skip_tests only skips running them; ONNX Runtime's own unit tests take a third of the
  # build and do not compile against musl (its math functions are not noexcept)
  onnxruntime_BUILD_UNIT_TESTS=OFF
)
args=(
  --build_dir "$work/build"
  --config Release
  --update --build --parallel
  --skip_tests
  --skip_submodule_sync
  --compile_no_warning_as_error
  # build.py turns on Microsoft's 1DS telemetry by default since 1.31 (it also needs glibc's
  # execinfo.h); ferraus sends nothing anywhere
  --no_telemetry
)
case "$target" in
  x86_64-apple-darwin)
    # same execution providers and deployment target as the Apple silicon build
    args+=(--use_coreml --osx_arch x86_64 --apple_deploy_target 13.4)
    ;;
  x86_64-unknown-linux-musl | aarch64-unknown-linux-musl)
    args+=(--cmake_generator Ninja)
    # musl has strtod_l/strtof_l but not strtoll_l/strtoull_l, which flatbuffers' locale-independent
    # parsing also needs (Alpine's onnxruntime package patches the same thing)
    defines+=(FLATBUFFERS_LOCALE_INDEPENDENT=0)
    ;;
  *)
    echo "build.sh does not know how to build ONNX Runtime for $target" >&2
    exit 2
    ;;
esac

python3 "$src/tools/ci_build/build.py" "${args[@]}" --cmake_extra_defines "${defines[@]}"

# Keep only the static libraries (object files and sources would make the CI cache gigabytes).
bin="$work/build/Release"
rm -rf "$out"
mkdir -p "$out"
(cd "$bin" && find . -name '*.a' -type f) | while read -r lib; do
  mkdir -p "$out/$(dirname "$lib")"
  cp "$bin/$lib" "$out/$lib"
done
printf 'onnxruntime %s\ntarget %s\nbuild.py %s --cmake_extra_defines %s\n' \
  "$version" "$target" "${args[*]}" "${defines[*]}" > "$out/BUILD-INFO.txt"
test -f "$out/libonnxruntime_common.a"
echo "ONNX Runtime $version for $target: $(find "$out" -name '*.a' | wc -l | tr -d ' ') static libraries in $out"
du -sh "$out"

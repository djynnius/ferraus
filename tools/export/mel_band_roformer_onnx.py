"""Export a Mel-Band RoFormer checkpoint to ONNX for ferraus.

Usage (the registry's ``separate/mel-roformer-kj`` entry)::

    pip install torch "bs-roformer>=0.4" onnx onnxruntime pyyaml
    curl -LO https://huggingface.co/KimberleyJSN/melbandroformer/resolve/main/MelBandRoformer.ckpt
    curl -LO https://raw.githubusercontent.com/ZFTurbo/Music-Source-Separation-Training/main/configs/KimberleyJensen/config_vocals_mel_band_roformer_kj.yaml
    python tools/export/mel_band_roformer_onnx.py \
        --checkpoint MelBandRoformer.ckpt --config config_vocals_mel_band_roformer_kj.yaml \
        --name separate/mel-roformer-kj --version 2024.08 --complement other \
        --license-spdx MIT --license-url https://huggingface.co/KimberleyJSN/melbandroformer \
        --commercial-use true --fp16-weights --out build/mel_roformer_kj
    python tools/export/parity.py roformer --kind mel --checkpoint MelBandRoformer.ckpt \
        --config config_vocals_mel_band_roformer_kj.yaml --onnx build/mel_roformer_kj/model.onnx

The mel-band gather and the overlapping-band mask averaging (scatter-add) are exported with
``Gather`` and ``ScatterElements(reduction=add)`` (opset 16+); see ``roformer_common.py``.
All options are shared with ``bs_roformer_onnx.py``.
"""

from bs_roformer_onnx import export

if __name__ == "__main__":
    export("mel")

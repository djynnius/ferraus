
## NVIDIA software (the CUDA image, ghcr.io/djynnius/ferraus:latest-cuda)

This image is built on `nvidia/cuda:13.0.1-cudnn-runtime-ubuntu24.04`. The NVIDIA CUDA and cuDNN
software in it is proprietary and is not covered by ferraus's licenses: it is governed by the
NVIDIA Deep Learning Container License (`/NGC-DL-CONTAINER-LICENSE` in this image, copied
beside this file when the base image provides it), the CUDA Toolkit End User License Agreement
(https://docs.nvidia.com/cuda/eula/), and the cuDNN Software License Agreement. By using this
image you accept those terms.

ferraus uses the NVIDIA libraries through ONNX Runtime's CUDA execution provider (MIT), installed
in `/data/cache/providers/onnxruntime-1.31.0-cuda/` with its `LICENSE` and
`ThirdPartyNotices.txt`.

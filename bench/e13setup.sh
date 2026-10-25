#!/usr/bin/env bash
# E13 setup: fetch the llama.cpp GGUF, build llama.cpp with CUDA, and make
# an SGLang venv, in parallel. Each part logs to logs/e13-<part>.log and
# touches run/e13-<part>.ok or run/e13-<part>.failed when done.
# usage: nohup setsid e13setup.sh [gguf] [llama] [sglang] > /dev/null 2>&1 &
B=/var/tmp/glarb-bench
cd "$B" || exit 1
export UV_CACHE_DIR=$B/cache/uv UV_PYTHON_INSTALL_DIR=$B/cache/python
export HF_HUB_DISABLE_XET=1 HF_HOME=$B/cache/hf
mark() { # PART STATUS
  rm -f "run/e13-$1.ok" "run/e13-$1.failed"
  if [ "$2" = 0 ]; then touch "run/e13-$1.ok"; else touch "run/e13-$1.failed"; fi
}
gguf() {
  local i
  for i in 1 2 3 4 5 6; do
    vllm-venv/bin/hf download ggml-org/gpt-oss-20b-GGUF gpt-oss-20b-MXFP4.gguf \
      --local-dir models/gpt-oss-20b-gguf && return 0
    echo "gguf: attempt $i failed"; sleep 10
  done
  return 1
}
llama() {
  local cuda=/opt/cuda
  [ -d llama.cpp/.git ] || git clone --depth 1 https://github.com/ggml-org/llama.cpp || return 1
  git -C llama.cpp log -1 --format='llama.cpp %h %cd' --date=short
  # The host nvcc (CUDA 12.9) does not support gcc 15. CMake's compiler
  # check ignores CMAKE_CUDA_FLAGS, so pass the override through nvcc's
  # environment. HOST_CXX overrides the host compiler (gcc 14 fallback).
  export NVCC_APPEND_FLAGS=-allow-unsupported-compiler
  rm -rf llama.cpp/build
  PATH=$cuda/bin:$PATH cmake -S llama.cpp -B llama.cpp/build -G Ninja \
    -DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=86 -DCMAKE_BUILD_TYPE=Release \
    -DLLAMA_CURL=OFF -DCMAKE_CUDA_COMPILER=$cuda/bin/nvcc \
    ${HOST_CXX:+-DCMAKE_CUDA_HOST_COMPILER=$HOST_CXX} &&
    PATH=$cuda/bin:$PATH cmake --build llama.cpp/build -j 24 --target llama-server
}
sglang() {
  [ -x sglang-venv/bin/python ] ||
    nix run nixpkgs#uv -- venv --python 3.12 sglang-venv || return 1
  VIRTUAL_ENV=$B/sglang-venv nix run nixpkgs#uv -- pip install --upgrade "sglang[all]" &&
    sglang-venv/bin/python -c 'import sglang, torch; print("sglang", sglang.__version__, "torch", torch.__version__, torch.version.cuda)'
}
for part in "${@:-gguf llama sglang}"; do
  for p in $part; do
    ( "$p" > "logs/e13-$p.log" 2>&1; mark "$p" $? ) &
  done
done
wait

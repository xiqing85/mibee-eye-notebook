# Runtime AI models (not tracked in git)

Download to this directory; every engine fails open without them.

| Path | Source (hf-mirror) | Size | License |
|---|---|---|---|
| `audio/yamnet.onnx` | `jafet21/yamnetonnx` | ~16 MB | Apache-2.0 |
| `audio/silero_vad.onnx` | `istupakov/silero-vad-onnx` | ~2.3 MB | MIT |
| `ocr/ch_PP-OCRv4_det_infer.onnx` | `SWHL/RapidOCR` `PP-OCRv4/` | ~4.7 MB | Apache-2.0 |
| `ocr/ppocrv5_mobile_rec.onnx` | `nathanfhh/PaddleOCR-ONNX` | ~16 MB | Apache-2.0 |

`ocr/ppocrv5_dict.txt` (shipped in git) is the v5 recognition dictionary.
`nanodet-m.onnx` above this directory is the pre-existing vision detector.

## Voice / LLM / TTS (feature `voice` / `llm`)

| Path | Source | Size | License |
|---|---|---|---|
| `voice/kws/`, `voice/paraformer/` | k2-fsa sherpa-onnx release `asr-multi-zh-hans` / keyword models (`kws.tar.bz2` untracked; extracted onnx files untracked) | ~250 MB | Apache-2.0 |
| `voice/melo/` | k2-fsa `tts-models` release `vits-melo-tts-zh_en` (extract verbatim; tarball + contents untracked) | ~165 MB | Apache-2.0 (code) |
| `llm/qwen3-0.6b-q8_0.gguf` | ModelScope `Qwen/Qwen3-0.6B-GGUF` (hf-mirror also mirrors Qwen GGUFs) | ~640 MB | Apache-2.0 |
| `vlm/qwen3-vl-2b-instruct-q4_k_m.gguf` + `vlm/mmproj-…-q8_0.gguf` | ModelScope `Qwen/Qwen3-VL-2B-Instruct-GGUF` | ~1056 + 424 MB | Apache-2.0 |

`sherpa-onnx` static libs for the `voice` feature build:
k2-fsa release `sherpa-onnx-v1.13.8-linux-x64-static-lib.tar.bz2`, build with
`SHERPA_ONNX_LIB_DIR=<extracted>/lib` (arm64 equivalents exist for the Pi
deployments). The `llm` feature needs AVX2-class CPUs (llama.cpp GGML kernels)
— Sandy Bridge and older hosts must build with `--features voice` only.

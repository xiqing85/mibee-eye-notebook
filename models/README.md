# Runtime AI models (not tracked in git)

Download to this directory; every engine fails open without them.

| Path | Source (hf-mirror) | Size | License |
|---|---|---|---|
| `audio/yamnet.onnx` | `jafet21/yamnetonnx` | ~16 MB | Apache-2.0 |
| `voice/speaker/campplus.onnx` | k2-fsa release `speaker-recongition-models` → `3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx` | ~27 MB | Apache-2.0 |
| `voice/paraformer-trilingual/` | `csukuangfj/sherpa-onnx-paraformer-trilingual-zh-cantonese-en` (`model.int8.onnx` + `tokens.txt`) | ~234 MB | Apache-2.0 |
| `audio/silero_vad.onnx` | `istupakov/silero-vad-onnx` | ~2.3 MB | MIT |
| `ocr/ch_PP-OCRv4_det_infer.onnx` | `SWHL/RapidOCR` `PP-OCRv4/` | ~4.7 MB | Apache-2.0 |
| `ocr/ppocrv5_mobile_rec.onnx` | `nathanfhh/PaddleOCR-ONNX` | ~16 MB | Apache-2.0 |

`ocr/ppocrv5_dict.txt` (shipped in git) is the v5 recognition dictionary.
`nanodet-m.onnx` above this directory is the pre-existing vision detector.

## Voice / LLM / TTS (feature `voice` / `llm`)

| Path | Source | Size | License |
|---|---|---|---|
| `voice/kws/`, `voice/paraformer/` | k2-fsa sherpa-onnx release `asr-multi-zh-hans` / keyword models (`kws.tar.bz2` untracked; extracted onnx files untracked) | ~250 MB | Apache-2.0 |

## Decision (feature `ai`, opt-in config `[decision]`)

| Path | Source | Size | License |
|---|---|---|---|
| `decision/laya_multilingual.int8.onnx` | self-exported via upstream `scripts/export_onnx.py --quantize` from `convaiinnovations/laya-multilingual` (mmBERT-base 322M, 100+ languages) | ~924 MB | Apache-2.0 |
| `decision/tokenizer.json`, `decision/laya_config.json` | from the same checkpoint (tokenizer + `rl_agent_config.json` distilled to max_len/head_max_len/temperatures) | ~18 MB | Apache-2.0 |

One-time export (torch CPU env): `pip install -e NandhaKishorM/laya` then
`python scripts/export_onnx.py --model convaiinnovations/laya-multilingual
--output laya_multilingual.onnx --quantize`. The fp32 export is ~1.3 GB;
the int8 copy quantizes MatMul weights only, so embeddings stay fp32 —
924 MB. Quality note: zero-shot triage is conservative (rarely answers
`ignore`); fine-tuning is where upstream accuracy jumps.

The default ASR checkpoint is paraformer-zh (Mandarin + embedded English).
For **Mandarin / Cantonese / English** recognition swap `[voice]` to the
trilingual checkpoint above: `paraformer_model =
"models/voice/paraformer-trilingual/model.int8.onnx"`, `paraformer_tokens =
"models/voice/paraformer-trilingual/tokens.txt"` — same engine, one file
pair. `voice/speaker/campplus.onnx` powers the voiceprint features
(verify gate + record attribution, SPEC appendix A #25); missing it only
disables those, wake + ASR keep working.
| `voice/melo/` | k2-fsa `tts-models` release `vits-melo-tts-zh_en` (extract verbatim; tarball + contents untracked) | ~165 MB | Apache-2.0 (code) |
| `llm/qwen3-0.6b-q8_0.gguf` | ModelScope `Qwen/Qwen3-0.6B-GGUF` (hf-mirror also mirrors Qwen GGUFs) | ~640 MB | Apache-2.0 |
| `vlm/qwen3-vl-2b-instruct-q4_k_m.gguf` + `vlm/mmproj-…-q8_0.gguf` | ModelScope `Qwen/Qwen3-VL-2B-Instruct-GGUF` | ~1056 + 424 MB | Apache-2.0 |

`sherpa-onnx` static libs for the `voice` feature build:
k2-fsa release `sherpa-onnx-v1.13.8-linux-x64-static-lib.tar.bz2`, build with
`SHERPA_ONNX_LIB_DIR=<extracted>/lib` (arm64 equivalents exist for the Pi
deployments). The `llm` feature needs AVX2-class CPUs (llama.cpp GGML kernels)
— Sandy Bridge and older hosts must build with `--features voice` only.

# Whisper models

Studio transcription uses [whisper.cpp](https://github.com/ggml-org/whisper.cpp) ggml models. Models
are downloaded at run time by `aun_media::transcribe::ModelManager`, are verified against the SHA-256
below, and are **never committed to this repository**.

All models are multilingual, because Arabic is a first-class language. The `.en` variants are left
out on purpose. Source: `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/<file>`.

| Model id | File | Size (bytes) | SHA-256 |
|---|---|---:|---|
| `tiny-q5_1` | `ggml-tiny-q5_1.bin` | 32,152,673 | `818710568da3ca15689e31a743197b520007872ff9576237bda97bd1b469c3d7` |
| `tiny-q8_0` | `ggml-tiny-q8_0.bin` | 43,537,433 | `c2085835d3f50733e2ff6e4b41ae8a2b8d8110461e18821b09a15c40c42d1cca` |
| `tiny` | `ggml-tiny.bin` | 77,691,713 | `be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21` |
| `base-q5_1` | `ggml-base-q5_1.bin` | 59,707,625 | `422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898` |
| `base-q8_0` | `ggml-base-q8_0.bin` | 81,768,585 | `c577b9a86e7e048a0b7eada054f4dd79a56bbfa911fbdacf900ac5b567cbb7d9` |
| `base` | `ggml-base.bin` | 147,951,465 | `60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe` |
| `small-q5_1` | `ggml-small-q5_1.bin` | 190,085,487 | `ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb` |
| `small-q8_0` | `ggml-small-q8_0.bin` | 264,464,607 | `49c8fb02b65e6049d5fa6c04f81f53b867b5ec9540406812c643f177317f779f` |
| `small` | `ggml-small.bin` | 487,601,967 | `1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b` |
| `medium-q5_0` | `ggml-medium-q5_0.bin` | 539,212,467 | `19fea4b380c3a618ec4723c3eef2eb785ffba0d0538cf43f8f235e7b3b34220f` |
| `medium-q8_0` | `ggml-medium-q8_0.bin` | 823,369,779 | `42a1ffcbe4167d224232443396968db4d02d4e8e87e213d3ee2e03095dea6502` |
| `medium` | `ggml-medium.bin` | 1,533,763,059 | `6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208` |
| `large-v3-turbo-q5_0` | `ggml-large-v3-turbo-q5_0.bin` | 574,041,195 | `394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2` |
| `large-v3-turbo-q8_0` | `ggml-large-v3-turbo-q8_0.bin` | 874,188,075 | `317eb69c11673c9de1e1f0d459b253999804ec71ac4c23c17ecf5fbe24e259a1` |
| `large-v3-turbo` | `ggml-large-v3-turbo.bin` | 1,624,555,275 | `1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69` |

Sizes and checksums come from the Hugging Face LFS metadata for `ggerganov/whisper.cpp`, retrieved
2026-11-08.

## Licences

| Component | Licence | Notes |
|---|---|---|
| OpenAI Whisper weights | MIT | [`openai/whisper` LICENSE](https://github.com/openai/whisper/blob/main/LICENSE) covers the code and the model weights |
| whisper.cpp and its ggml conversions | MIT | [`ggml-org/whisper.cpp` LICENSE](https://github.com/ggml-org/whisper.cpp/blob/master/LICENSE) |
| `whisper-rs` and `whisper-rs-sys` | Unlicense | Only built with the `whisper` feature, so outside the default `cargo deny` graph; see below |

`deny.toml` checks the default feature set. `whisper-rs` is off by default and its Unlicense is
permissive, but the allow-list would need `Unlicense` added if the licence check is ever extended to
all features.

## Test audio

No recorded speech is committed. `.github/workflows/media.yml` generates short English and Arabic
clips at test time with `espeak-ng`. Audio synthesised by espeak-ng is program output and does not
carry the GPL licence of espeak-ng itself. The whisper inference tests use the `tiny` model, which
the workflow caches and checks against the checksum above.

# 3DS Z3DS benchmark (Windows, x64)

rom-converto versus `z3ds_compressor` (Azahar Z3DS CLI) on decrypted
3DS ROMs, Z3DS container. These numbers are specific to the Windows x64
host described below.

## Host

- AMD Ryzen 9 5900X, 12 cores / 24 threads, 64 GB RAM.
- Windows 11 Pro (build 26200), x86_64.
- z3ds_compressor, Windows build from the upstream `corruption_fix`
  release.
- rom-converto built with `cargo build --release`.

## Methodology

- **N = 5** runs per metric, **interleaved execution**: one external
  tool run, 3 s cooldown, one rom-converto run, 3 s cooldown, repeat.
- A residual process kill runs before every invocation so no leftover
  process or modal error dialog can leak state across runs.
- **Warm stats exclude run 1** (cold cache).
- Delta is the rom-converto / external ratio; values below 1.00x mean
  rom-converto is faster.
- After every compress, the external tool's output is decompressed with
  `rom-converto ctr decompress` and its SHA-256 compared to the source
  file, as a format-compatibility check.
- `z3ds_compressor` is compress only, so decompress rows report
  rom-converto's timing solo with no external column.
- Both inputs are decrypted with `rom-converto ctr decrypt` first.
  Compression on encrypted 3DS content has near zero ratios, so the
  decrypted form is the fair input.
- Harness: `rom-converto-benchmark ctr --three-ds <3ds> --cia <cia>`.

Inputs: a 1.0 GB decrypted cart image (NCSD format) and a 645 MiB
decrypted CIA (CTR package format).

## Results

| Operation | external warm mean | rom-converto warm mean | Delta | Size delta |
|---|---:|---:|---:|---:|
| .3ds compress | 2.598 s (sigma = 0.084) | **0.736 s (sigma = 0.025)** | **0.28x (3.53x faster)** | -16,352 B (-0.0031 %) |
| .3ds decompress | - | **0.912 s (sigma = 0.025)** | - | - |
| .cia compress | 1.840 s (sigma = 0.054) | **0.737 s (sigma = 0.053)** | **0.40x (2.50x faster)** | **-27,869,133 B (-4.9046 %)** |
| .cia decompress | - | **0.693 s (sigma = 0.013)** | - | - |

The cross-tool check passed on both inputs: the external tool's output
decompresses through `rom-converto ctr decompress` to bytes whose
SHA-256 matches the source.

# GameCube RVZ benchmark (Windows, x64)

rom-converto versus `DolphinTool` 2632 on GameCube disc images, default
128 KiB chunk size, zstd level 5 and level 22. These numbers are
specific to the Windows x64 host described below.

## Host

- AMD Ryzen 9 5900X, 12 cores / 24 threads, 64 GB RAM.
- Windows 11 Pro (build 26200), x86_64.
- DolphinTool 2632, x64 build.
- rom-converto built with `cargo build --release`.

## Methodology

- **N = 5** runs per metric, **interleaved execution**: one Dolphin run,
  3 s cooldown, one rom-converto run, 3 s cooldown, repeat.
- A residual process kill runs before every invocation so no leftover
  process or modal error dialog can leak state across runs.
- **Warm stats exclude run 1** (cold cache).
- Delta is the rom-converto / Dolphin ratio; values below 1.00x mean
  rom-converto is faster.
- Compress runs at zstd level 5 and level 22, 128 KiB chunk size.
  Decompress reads an existing RVZ and writes a raw ISO.
- After every compress, `DolphinTool header -i <our.rvz>` runs as a
  parse sanity check.
- Harness: `rom-converto-benchmark gamecube --iso <iso>`.

Input: one 1.4 GB GameCube disc image.

## Results

| Operation | Dolphin warm mean | rom-converto warm mean | Delta | Size delta |
|---|---:|---:|---:|---:|
| Compress L5 | 1.109 s (sigma = 0.094) | 1.234 s (sigma = 0.160) | 1.11x | -11,612 B (-0.0012 %) |
| Compress L22 | 11.358 s (sigma = 1.095) | **9.193 s (sigma = 0.158)** | **0.81x (1.24x faster)** | -16,724 B (-0.0018 %) |
| Decompress | 1.958 s (sigma = 0.096) | **1.096 s (sigma = 0.184)** | **0.56x (1.79x faster)** | - |

`DolphinTool header` accepts the rom-converto output.

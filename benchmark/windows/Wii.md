# Wii RVZ benchmark (Windows, x64)

rom-converto versus `DolphinTool` 2632 on Wii disc images, default
128 KiB chunk size, zstd level 5 and level 22. These numbers are
specific to the Windows x64 host described below.

## Host

- AMD Ryzen 9 5900X, 12 cores / 24 threads, 64 GB RAM.
- Windows 11 Pro (build 26200), x86_64. AES-NI auto-detected by the
  `aes` crate.
- DolphinTool 2632, x64 build.
- rom-converto built with `cargo build --release`.

## Methodology

- **N = 7** runs per metric, **interleaved execution**: one Dolphin run,
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
- Harness: `rom-converto-benchmark wii --iso <iso>`.

Input: one 4.4 GB Wii disc image with encrypted partitions.

## Results

| Operation | Dolphin warm mean | rom-converto warm mean | Delta | Size delta |
|---|---:|---:|---:|---:|
| Compress L5 | 4.200 s (sigma = 0.366) | 4.959 s (sigma = 0.407) | 1.18x | -60,272 B (-0.0017 %) |
| Compress L22 | 33.259 s (sigma = 2.440) | **32.715 s (sigma = 2.071)** | **0.98x (1.02x faster)** | +4,376 B (+0.0001 %) |
| Decompress | 7.741 s (sigma = 0.290) | **7.573 s (sigma = 0.312)** | **0.98x (1.02x faster)** | - |

`DolphinTool header` accepts the rom-converto output.

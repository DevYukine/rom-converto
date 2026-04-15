# Wii RVZ benchmark

rom-converto versus `DolphinTool.exe` 2603a-x64 on Wii disc images,
default 128 KiB chunk size, zstd level 5 and level 22.

## Methodology

- **N = 10** runs per metric, **interleaved execution**: one Dolphin run,
  3 s cooldown, one rom-converto run, 3 s cooldown, repeat.
- `taskkill /F /IM DolphinTool.exe rom-converto.exe` between every run so
  no modal error dialog or zombie process can leak state across runs.
- **Warm stats exclude run 1** (cold cache).
- Δ is the rom-converto / Dolphin ratio; values below 1.00× mean
  rom-converto is faster.
- After every compress, `DolphinTool.exe header -i <our.rvz>` runs as a
  parse sanity check.
- Harness: `benchmark/dolphin_benchmark.py` (not committed, host
  dependent).

Input: one 4.38 GB Wii test disc with two encrypted partitions.

## Results

| Operation | Dolphin warm mean | rom-converto warm mean | Δ | Size delta |
|---|---:|---:|---:|---:|
| Compress L5 | 2.575 s (σ = 0.156) | 2.762 s (σ = 0.107) | **1.07×** | −63,268 B (−0.0029 %) |
| Compress L22 | 20.411 s (σ = 0.817) | 21.040 s (σ = 2.969) | **1.03×** | −62,132 B (−0.0029 %) |
| Decompress | 6.664 s (σ = 0.188) | **6.455 s (σ = 0.159)** | **0.97×** | - |

`DolphinTool.exe header` accepts every compressed output.

## Interpretation

Every Wii operation is within 7 % of Dolphin; decompress is slightly
faster. Compression ratios are marginally better than Dolphin's on both
L5 and L22, within zstd framing noise.

The Wii path is more work per cluster than GameCube (SHA-1 + AES +
hash hierarchy recompute + exception list build), so the fixed dispatcher
overhead that hurts GC L5 gets amortised across more compute and the
ratio stays close to 1.0× even at L5.

## Cross-tool parity

The gated integration test
`nintendo::rvz::integration_tests::dolphin_parity_cross_tool_round_trip`
runs a four-step bidirectional round trip for every ISO set via env
var, comparing SHA-1 digests against the original disc. Both
directions are byte identical for Wii inputs, including partitions
whose declared `data_size` is not a multiple of the 2 MiB cluster
size (partial last cluster, padding sectors in the following raw
region).

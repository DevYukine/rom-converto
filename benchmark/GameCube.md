# GameCube RVZ benchmark

rom-converto versus `DolphinTool.exe` 2603a-x64 on GameCube disc images,
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

Input: one 1.46 GB GameCube test disc.

## Results

| Operation | Dolphin warm mean | rom-converto warm mean | Δ | Size delta |
|---|---:|---:|---:|---:|
| Compress L5 | 0.762 s (σ = 0.037) | 0.947 s (σ = 0.013) | 1.24× | −11,612 B (−0.0012 %) |
| Compress L22 | 7.849 s (σ = 0.365) | 7.921 s (σ = 0.376) | **1.01×** | −16,724 B (−0.0018 %) |
| Decompress | 1.806 s (σ = 0.059) | **0.740 s (σ = 0.039)** | **0.41×** (2.4× faster) | — |

`DolphinTool.exe header` accepts every compressed output.

## Interpretation

- **Decompress** is 2.4× faster than Dolphin.
- **Compress L22** is at parity with Dolphin (1 % gap, well inside run
  to run variance).
- **Compress L5** is the remaining outlier. L5 compression finishes in
  under a second, so any dispatcher overhead shows up as a double digit
  percentage even when the absolute gap is ~200 ms. The underlying bottleneck
  is sequential `read_exact` on the dispatcher thread; both a dedicated
  reader thread and `Mmap` input were tested and each measured worse on
  Windows (extra channel hops and page fault latency respectively).

## Cross-tool parity

The gated integration test
`nintendo::rvz::integration_tests::dolphin_parity_cross_tool_round_trip`
runs a four-step bidirectional round trip for every ISO set via env
var, comparing SHA-1 digests against the original disc. Both
directions are byte identical for GameCube inputs.

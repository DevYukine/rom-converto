# CHD benchmark (macOS, Apple Silicon)

rom-converto versus `chdman` 0.288 on a CD image, default codecs on
both sides. These numbers are specific to the macOS Apple Silicon host
described below. The Windows reference numbers live in
[`../CHD.md`](../CHD.md).

## Host

- Apple M4, 10 cores, 16 GB unified memory.
- macOS 26.3 (build 25D125), arm64.
- chdman 0.288, installed through Homebrew.
- rom-converto built with `cargo build --release`.

## Methodology

- **N = 10** runs per metric, **interleaved execution**: one chdman run,
  3 s cooldown, one rom-converto run, 3 s cooldown, repeat.
- A residual process kill runs before every invocation so no leftover
  process can leak state across runs.
- **Warm stats exclude run 1** (cold cache).
- Delta is the rom-converto / chdman ratio; values below 1.00x mean
  rom-converto is faster.
- After every compress, `chdman info -i <our.chd>` runs as a parse
  sanity check.
- Harness: `rom-converto-benchmark chd --cue <cue>`. The cue path points
  at a `.cue` with a sibling `.bin`.

Input: one 543 MB CD image, single MODE2/2352 track, `.bin` plus `.cue`
pair.

## Results

| Operation | chdman warm mean | rom-converto warm mean | Delta | Size delta |
|---|---:|---:|---:|---:|
| CD compress | 7.851 s (sigma = 0.022) | **6.050 s (sigma = 0.085)** | **0.77x (1.30x faster)** | **-4 B (-0.0000 %)** |
| CD extract | 10.171 s (sigma = 0.099) | **1.593 s (sigma = 0.071)** | **0.16x (6.39x faster)** | +0 B (+0.0000 %) |
| CD verify | 11.813 s (sigma = 0.025) | **1.664 s (sigma = 0.087)** | **0.14x (7.10x faster)** | - |

`chdman info` accepts the rom-converto output.

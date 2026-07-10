# CHD benchmark (Windows, x64)

rom-converto versus `chdman` 0.284 on a CD image, default codecs on
both sides. These numbers are specific to the Windows x64 host
described below.

## Host

- AMD Ryzen 9 5900X, 12 cores / 24 threads, 64 GB RAM.
- Windows 11 Pro (build 26200), x86_64. AES-NI auto-detected by the
  `aes` crate.
- chdman 0.284 (mame0284).
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

Input: one 570 MB CD image, single MODE2/2352 track, `.bin` plus `.cue`
pair.

## Results

| Operation | chdman warm mean | rom-converto warm mean | Delta | Size delta |
|---|---:|---:|---:|---:|
| CD compress | 7.082 s (sigma = 0.223) | **4.283 s (sigma = 0.125)** | **0.60x (1.65x faster)** | +0 B (+0.0000 %) |
| CD extract | 13.179 s (sigma = 0.335) | **0.764 s (sigma = 0.037)** | **0.06x (17.24x faster)** | +0 B (+0.0000 %) |
| CD verify | 16.174 s (sigma = 0.143) | **0.668 s (sigma = 0.027)** | **0.04x (24.22x faster)** | - |

`chdman info` accepts the rom-converto output.

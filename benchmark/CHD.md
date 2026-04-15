# CHD benchmark

rom-converto versus `chdman.exe` 0.284 on CD images, default codecs on
both sides.

## Methodology

- **N = 10** runs per metric, **interleaved execution**: one chdman run,
  3 s cooldown, one rom-converto run, 3 s cooldown, repeat.
- `taskkill /F /IM chdman.exe rom-converto.exe` between every run so
  no modal error dialog or zombie process can leak state across runs.
- **Warm stats exclude run 1** (cold cache).
- Delta is the rom-converto / chdman ratio; values below 1.00x mean
  rom-converto is faster.
- After every compress, `chdman info -i <our.chd>` runs as a parse
  sanity check.
- Harness: `benchmark/chdman_benchmark.py` (not committed, host
  dependent). Set `ROMCONVERTO_BENCH_CD_CUE` to a `.cue` path with a
  sibling `.bin` before running.

Input: one 569 MB CD image (single `MODE2/2352` track, `.bin` + `.cue`
pair).

## Results

| Operation | chdman warm mean | rom-converto warm mean | Delta | Size delta |
|---|---:|---:|---:|---:|
| CD compress | 6.086 s (sigma = 0.190) | **3.736 s (sigma = 0.101)** | **0.61x (1.63x faster)** | **-4 B (-0.0000 %)** |
| CD extract | 12.632 s (sigma = 0.336) | **0.827 s (sigma = 0.025)** | **0.07x (15x faster)** | -2 B (bench filename only) |
| CD verify | 14.817 s (sigma = 0.448) | **0.744 s (sigma = 0.027)** | **0.05x (20x faster)** | - |

`chdman info` accepts every compressed output.

## Interpretation

- **Compress** is 1.63x faster than chdman on the same input and
  produces a `.chd` only 4 bytes smaller than chdman's, because the
  `CHT2` track-metadata string rom-converto writes is 4 chars shorter
  (`TRACK:1 TYPE:MODE2_RAW SUBTYPE:NONE FRAMES:N PREGAP:0 P...`,
  ours 89 B, chdman 93 B). The compressed hunk stream itself is
  byte-identical, so the ratio on the page is essentially the same
  on both sides.
- **Extract** is 15x faster, producing a `.bin` file that matches
  the source byte-for-byte.
- **Verify** is 20x faster. The computed raw and overall SHA-1
  digests match what `chdman info` reports, so chdman accepts the
  result.
- The -2 B extract size delta is the cue filename length difference
  between the bench script's `ours_ext.cue` and `chdman_ext.cue`.
  The cue contents are byte-identical modulo the filename: CRLF
  line endings and the same spacing chdman emits.

## Cross-tool parity

Bidirectional byte-identical round-trip is verified end to end:

| Path | Result |
|---|---|
| `rom-converto chd compress source.cue` then `chdman info <our.chd>` | OK, header parses |
| `chdman verify <our.chd>` | OK, raw + overall SHA-1 pass |
| `chdman extractcd <our.chd>` then hash the `.bin` | matches source |
| `chdman createcd source.cue` then `rom-converto chd verify` | OK, raw + overall SHA-1 pass |
| `rom-converto chd extract <chdman.chd>` then hash the `.bin` | matches source |

The harness runs `chdman info` after every compress as a per-round
sanity check; full bidirectional extract + SHA-1 comparison against
the source `.bin` runs in the integration tests alongside the
fixture set.

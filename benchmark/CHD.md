# CHD benchmark

rom-converto versus `chdman.exe` 0.284 on CD images, default codecs on
both sides.

## Methodology

- **N = 10** runs per metric, **interleaved execution**: one chdman run,
  3 s cooldown, one rom-converto run, 3 s cooldown, repeat.
- `taskkill /F /IM chdman.exe rom-converto.exe` between every run so
  no modal error dialog or zombie process can leak state across runs.
- **Warm stats exclude run 1** (cold cache).
- Δ is the rom-converto / chdman ratio; values below 1.00× mean
  rom-converto is faster.
- After every compress, `chdman info -i <our.chd>` runs as a parse
  sanity check.
- Harness: `benchmark/chdman_benchmark.py` (not committed, host
  dependent). Set `ROMCONVERTO_BENCH_CD_CUE` to a `.cue` path with a
  sibling `.bin` before running.

Input: one 569 MB CD image (single `MODE2/2352` track, `.bin` + `.cue`
pair).

## Results

| Operation | chdman warm mean | rom-converto warm mean | Δ | Size delta |
|---|---:|---:|---:|---:|
| CD compress | 6.086 s (σ = 0.190) | **3.736 s (σ = 0.101)** | **0.61×** (1.63× faster) | **-4 B (−0.0000 %)** |
| CD extract | 12.632 s (σ = 0.336) | **0.827 s (σ = 0.025)** | **0.07×** (15× faster) | -2 B (bench filename only) |
| CD verify | 14.817 s (σ = 0.448) | **0.744 s (σ = 0.027)** | **0.05×** (20× faster) | - |

`chdman info` accepts every compressed output.

## Interpretation

- **Compress** is 1.63× faster than chdman and byte-parity on the
  compressed hunk stream. The remaining -4 B file-size delta is
  purely cosmetic: our `CHT2` track-metadata string is 4 chars
  shorter than chdman's (`TRACK:1 TYPE:MODE2_RAW SUBTYPE:NONE
  FRAMES:N PREGAP:0 P...`, ours 89 B, chdman 93 B), compression
  matches bit-for-bit. Switching the `flate2` backend from `zlib-ng`
  to upstream `zlib` was the fix: `zlib-ng` picks slightly different
  deflate block structures at level 9, enough that our CDLZ
  (LZMA base + deflate subcode) beat our CDZL (deflate + deflate) on
  ~2.5K hunks where chdman's CDZL beat its CDLZ. Once the deflate
  output matched chdman exactly, codec selection matched too, and
  compression got *faster* because LZMA is much slower than deflate,
  so correctly picking CDZL more often is a net speedup.
- **Extract** is 15× faster. The pipeline is a 16-thread worker pool
  sharing one `Arc<File>` via Windows `seek_read` / Unix `read_at`
  positional reads, with persistent LZMA + deflate decoder state per
  worker and one batched `write_all` per hunk (versus 242,222
  per-frame writes in the old serial path).
- **Verify** is 20× faster. Same pipeline as extract minus the bin
  write, so the consume closure just folds each decoded hunk into a
  rolling SHA-1. Workers decode in parallel, the dispatcher hashes
  in order.
- The -2 B extract size delta is the cue filename length difference
  between the bench script's `ours_ext.cue` and `chdman_ext.cue`.
  The cue contents are byte-identical modulo the filename (CRLF line
  endings + exact spacing matching chdman's
  `output_track_metadata`).

## Cross-tool parity

Bidirectional byte-identical round-trip is verified end to end:

| Path | Result |
|---|---|
| `ours compress source.cue → ours.chd` | ✅ |
| `chdman info ours.chd` | ✅ header parses |
| `chdman verify ours.chd` | ✅ raw + overall SHA-1 pass |
| `chdman extractcd ours.chd → bin.sha1` | **matches source** ✅ |
| `chdman createcd source.cue → chdman.chd` | ✅ |
| `ours verify chdman.chd` | ✅ raw + overall SHA-1 pass |
| `ours extract chdman.chd → bin.sha1` | **matches source** ✅ |

The harness runs `chdman info` after every compress as a per-round
sanity check; full bidirectional extract + SHA-1 comparison against
the source `.bin` runs in the integration tests alongside the
fixture set.

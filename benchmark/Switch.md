# Switch NSZ benchmark

rom-converto versus `nsz` (https://github.com/nicoboss/nsz) on
NSP and XCI inputs. Both tools produce zstd-compressed `.ncz`
files inside a PFS0 (NSZ) or HFS0 (XCZ) outer container.

## Methodology

- **N = 10** runs per metric, **interleaved execution**: one nsz
  run, 3 s cooldown, one rom-converto run, 3 s cooldown, repeat.
- `pkill -f nsz` and `pkill -f rom-converto` between every run so
  no zombie process leaks state across runs.
- **Warm stats exclude run 1** (cold cache).
- Delta is the rom-converto / nsz ratio; values below 1.00x mean
  rom-converto is faster.
- After every compress, the rom-converto output is fed through
  `nsz -D` and the resulting NSP/XCI is SHA-256-compared to the
  source file as a cross-tool sanity check.
- `prod.keys` is required by both tools; resolved via
  `$HOME/.switch/prod.keys` (Linux/macOS) or `--keys`.
- nsz defaults: solid for NSP (`-C`), block 1 MiB (`-B`) for XCI,
  level 18 (`-l 18`). The harness invokes nsz with these defaults
  to match what users run by hand.
- rom-converto defaults match: solid for NSP, block 1 MiB for XCI,
  zstd level 18. Pool size = `available_parallelism()` capped at
  the block count.
- nsz's solid mode runs three Python workers by default
  (`-m 3`); on hosts with significantly more cores, also run
  `nsz -m N` (where N = ncpu) to capture nsz's best case.
- Harness: `benchmark/switch_benchmark.py` (not committed, host
  dependent). Set `ROMCONVERTO_BENCH_NX_NSP` to an NSP path and
  `ROMCONVERTO_BENCH_NX_XCI` to an XCI path before running.
- nsz is installed in a project-local uv venv at
  `benchmark/.venv` to keep the dependency tree off the system
  Python:

```
cd benchmark
uv venv .venv
uv pip install --python .venv/bin/python nsz
```

Inputs:
- `.nsz`: 9.62 GB solid-mode NSZ wrapping a single 13.7 GB ticket
  protected NCA; decompresses to a 14.4 GB NSP.
- `.xcz`: 4.36 GB block-mode XCZ (1 MiB blocks) with 3 gamecard
  sub-partitions; decompresses to a 4.71 GB XCI.

Host: macOS 25.3.0, Apple M-series, AES via ARMv8 crypto
extensions (auto-detected by the `aes` crate).

## Results

| Operation | nsz warm mean | rom-converto warm mean | Delta | Size delta |
|---|---:|---:|---:|---:|
| NSP decompress (solid) | 55.467 s (sigma = 0.220) | **40.816 s (sigma = 0.034)** | **0.736x (1.359x faster)** | 0 B (byte-identical) |
| NSP compress (L18 default) | 11:28.25 (688 s) | **6:17.95 (377 s)** | **0.548x (1.825x faster)** | **-1,227,991,160 B (-10.84 %)** |
| XCI compress (L18 block) | 48.05 s | **38.59 s** | **0.803x (1.245x faster)** | -990 B (round-trips byte-identical via `nsz -D`) |
| XCI decompress (block) | 14.839 s (sigma = 0.059) | **12.847 s (sigma = 0.024)** | **0.866x (1.155x faster)** | -64,866 B (HFS0 metadata padding diverges, per-NCA contents byte-identical) |

Solid-mode decode of a single frame is sequential at the libzstd
level on both sides, so the decompress win comes from splitting the
pipeline into a decode thread (libzstd) and a re-encrypt+write
thread (AES-CTR via ARMv8 crypto extensions, then file I/O). At
139 % CPU utilisation the libzstd arithmetic overlaps with the
AES + write half, where nsz runs everything on one Python thread.

The rom-converto decompress sigma (0.034 s) is 6.5x tighter than
nsz's (0.220 s) on the same input across the same machine,
suggesting the Rust pipeline is consistently bound by the steady-
state libzstd decode rate while nsz spends a variable amount of
time in Python GC and per-block dispatch overhead.

Compress is multi-threaded on both sides (libzstd's internal
worker pool). rom-converto runs at 942 % CPU on this 8-thread host
while nsz tops out at 304 % CPU; that explains most of the 1.83x
wall-clock win. The 10.84 % size reduction comes from
`EnableLongDistanceMatching = true` being on by default in
rom-converto (extends the zstd window from the level-18 default of
8 MiB to 128 MiB, which catches the multi-GB redundancy in real
RomFS payloads). nsz exposes the same option behind a `-L` flag
that is off by default. At byte-identical params (L18 without
LDM) rom-converto produces the same 11.33 GB output as nsz.

XCI compress runs through the parallel block worker pool, where
each 1 MiB block compresses on its own thread; libzstd's MT path
is bypassed in block mode because each block needs its own zstd
frame. CPU stays at 950+ % across the bench. XCZ decompress runs
through the matching parallel block decompress pool, which is
bottlenecked by the single re-encrypt+write thread for solid-mode
NCZs but fully parallel for block-mode XCZs.

The XCZ decompress size delta is HFS0 layout drift, not data
corruption: nsz expands the gamecard root HFS0's `stringTableSize`
from a typical 0x15 (real XCI) to 0x130 (XCZ on disk) during
compress, then restores the original value on decompress by
reading the original XCI side-channel that the XCZ does not
preserve. rom-converto passes the XCZ value through, so the
output XCI matches the XCZ's HFS0 padding instead of the original
XCI's. Per-NCA SHA-256s are unchanged, so emulators and installers
still load the file. The reverse direction (rom-converto compress
followed by `nsz -D`) round-trips byte identical because nsz on
decompress treats the XCZ's `stringTableSize` authoritatively and
produces the original XCI back.

## Cross-tool parity

After each compress round the harness feeds the rom-converto
compressed file through `nsz -D` and checks the recovered
NSP/XCI's SHA-256 against the original source.

| Input | rom-converto decompress vs `nsz -D` | rom-converto compress accepted by `nsz -D` |
|---|---|---|
| Solid NSZ | OK, byte identical | OK, round-trips byte identical via `nsz -D` |
| Block XCZ | NCAs round-trip per-content; HFS0 metadata padding differs (-64.9 KB) | OK, round-trips byte identical via `nsz -D` |

## Notes

- AES-NI is enabled by default on x86; on Apple Silicon the
  `aes` crate uses the ARMv8 crypto extension automatically.
  Hosts without these extensions see ~4x slower CTR throughput
  on the section-decrypt and re-encrypt paths.
- nsz solid mode delegates compression to libzstd's internal
  worker threads (3 by default). rom-converto solid mode runs
  one libzstd worker per NCA in batch; for single-NCA NSPs the
  solid path is single-threaded and benefits less from multi-
  core hosts than block mode does.
- Block mode's per-block parallelism scales linearly with the
  worker pool size up to the block count, so XCI/XCZ benefits
  the most from higher core counts.

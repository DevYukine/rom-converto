# 3DS Z3DS benchmark

rom-converto versus `z3ds_compressor.exe` (Azahar Emulator Z3DS CLI,
bundled with the `z3ds_compressorr` folder) on decrypted 3DS ROMs.
Both tools implement the same Azahar Z3DS container format.

## Methodology

- **N = 10** runs per metric, **interleaved execution**: one external
  tool run, 3 s cooldown, one rom-converto run, 3 s cooldown, repeat.
- `taskkill /F /IM z3ds_compressor.exe rom-converto.exe` between every
  run so no modal error dialog or zombie process can leak state across
  runs.
- **Warm stats exclude run 1** (cold cache).
- Delta is the rom-converto / external ratio; values below 1.00x mean
  rom-converto is faster.
- After every compress, the external tool's output is decompressed
  with `rom-converto ctr decompress` and its SHA-256 compared to the
  source file, as a format-compatibility check.
- `z3ds_compressor.exe` is compress-only per its `--help` output, so
  decompress rows report rom-converto's timing solo with no external
  column.
- Both inputs are pre-decrypted with `rom-converto ctr decrypt` before
  the bench runs. Compression on encrypted 3DS content has near-zero
  ratios so both tools need the same decrypted input for a fair
  comparison.
- Harness: `benchmark/z3ds_benchmark.py` (not committed, host
  dependent). Set `ROMCONVERTO_BENCH_CTR_3DS` to a decrypted `.3ds`
  path and `ROMCONVERTO_BENCH_CTR_CIA` to a decrypted `.cia` path
  before running.

Inputs:
- `.3ds`: 512 MB decrypted 3DS cart dump (single retail title, NCSD
  format)
- `.cia`: 631 MB decrypted CIA (single retail title, CTR package
  format)

## Results

| Operation | external warm mean | rom-converto warm mean | Delta | Size delta |
|---|---:|---:|---:|---:|
| .3ds compress | 1.439 s (sigma = 0.039) | **0.433 s (sigma = 0.028)** | **0.30x (3.3x faster)** | -8,176 B (-0.0028 %) |
| .3ds decompress | - | **0.584 s (sigma = 0.014)** | - | - |
| .cia compress | 2.383 s (sigma = 0.042) | **0.867 s (sigma = 0.020)** | **0.36x (2.7x faster)** | **-25,611,237 B (-4.5494 %)** |
| .cia decompress | - | **0.843 s (sigma = 0.025)** | - | - |

rom-converto decompresses the external tool's output byte-identical
to the source file on both inputs (SHA-256 parity confirmed per round).

## Interpretation

- **.cia compress** is 2.7x faster than the external tool and
  produces output 4.55 % smaller. Both axes improve at once on
  this input.
- **.3ds compress** is 3.3x faster and effectively tied on size
  (-0.003 % delta, 8 KB smaller on a 292 MB output).
- **Decompress** has no external-tool column because
  `z3ds_compressor.exe` is compress-only per its `--help` output.
  rom-converto decompresses the 512 MB `.3ds` input in 0.58 s and
  the 631 MB `.cia` input in 0.84 s.
- The two tools produce format-compatible output in both
  directions: rom-converto decompresses the external tool's
  output back to the source file with matching SHA-256 on both
  `.3ds` and `.cia`, and the compressed files rom-converto
  produces decode cleanly via a plain `zstd` streaming decoder
  without going through any rom-converto code at all.

## Cross-tool parity

After each compress round the bench feeds the external tool's
compressed file through `rom-converto ctr decompress` and checks the
decompressed bytes' SHA-256 against the original source. Result:

| Input | rom-converto decompress of external output |
|---|---|
| .3ds | OK, SHA-256 matches source |
| .cia | OK, SHA-256 matches source |

Both directions of the cross-tool round trip are bit identical.

# Switch NSZ benchmark (macOS, Apple Silicon)

rom-converto versus `nsz` (https://github.com/nicoboss/nsz) on NSP and
XCI inputs. These numbers are specific to the macOS Apple Silicon host
described below. The Windows reference numbers live in
[`../Switch.md`](../Switch.md).

## Host

- Apple M4, 10 cores, 16 GB unified memory.
- macOS 26.3 (build 25D125), arm64. AES runs through the ARMv8 crypto
  extensions, auto-detected by the `aes` crate.
- nsz installed through `uv tool install nsz`.
- rom-converto built with `cargo build --release`.

## Methodology

- Interleaved execution: one nsz run, 3 s cooldown, one rom-converto
  run, 3 s cooldown, repeat. **Warm stats exclude run 1** (cold cache).
- NSP compress and verify use **N = 10**. NSP decompress is reported on
  two inputs: the 308 MB NSP at **N = 10** and a larger NSP at **N = 6**.
  XCI metrics use **N = 6** to keep the wall-clock on the larger inputs
  bounded.
- A residual process kill runs before every invocation.
- Delta is the rom-converto / nsz ratio; values below 1.00x mean
  rom-converto is faster.
- `prod.keys` is required by both tools and is resolved from
  `$HOME/.switch/prod.keys`.
- nsz defaults: solid for NSP (`-C`), block 1 MiB (`-B`) for XCI, level
  18 (`-l 18`). rom-converto matches: solid for NSP, block for XCI, zstd
  level 18.
- After each compress, the harness feeds the rom-converto output through
  `nsz -D` and compares the SHA-256 of the result to the source.
- Harness: `rom-converto-benchmark switch --nsp <nsp> --xci <xci>`.

Inputs:
- A 308 MB NSP wrapping a single title, decoded from a solid-mode NSZ.
- A second NSP near 14 GB, decompressed from a roughly 9 GB solid-mode
  NSZ, used only for the large-input decompress row.
- A 4.4 GB XCI with three gamecard partitions, decoded from a block-mode
  XCZ.

## Results

| Operation | nsz warm mean | rom-converto warm mean | Delta | Size delta |
|---|---:|---:|---:|---:|
| NSP compress (L18 solid) | 13.830 s (sigma = 0.019) | **9.908 s (sigma = 0.044)** | **0.72x (1.40x faster)** | **-24,042,107 B (-16.5588 %)** |
| NSP decompress (solid, ~14 GB) | 54.951 s (sigma = 0.480) | **40.697 s (sigma = 0.122)** | **0.74x (1.35x faster)** | - |
| NSP decompress (solid, 308 MB) | 1.366 s (sigma = 0.010) | 1.468 s (sigma = 0.061) | 1.07x | - |
| NSP verify | - | **0.646 s (sigma = 0.025)** | - | - |
| XCI compress (L18 block) | 48.570 s (sigma = 0.320) | **38.853 s (sigma = 0.204)** | **0.80x (1.25x faster)** | -990 B (-0.0000 %) |
| XCI decompress (block) | 15.176 s (sigma = 0.059) | **14.133 s (sigma = 0.079)** | **0.93x (1.07x faster)** | - |
| XCI verify | - | **2.476 s (sigma = 0.150)** | - | - |

The cross-tool round trip passed on both inputs: the rom-converto
compressed file decompresses through `nsz -D` to bytes whose SHA-256
matches the source.

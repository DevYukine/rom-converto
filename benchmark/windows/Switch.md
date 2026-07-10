# Switch NSZ benchmark (Windows, x64)

rom-converto versus `nsz` (https://github.com/nicoboss/nsz) on NSP and
XCI inputs. These numbers are specific to the Windows x64 host described
below.

## Host

- AMD Ryzen 9 5900X, 12 cores / 24 threads, 64 GB RAM.
- Windows 11 Pro (build 26200), x86_64. AES runs through AES-NI,
  auto-detected by the `aes` crate.
- nsz 4.6.1, installed through `uv tool install nsz`.
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
  `%USERPROFILE%\.switch\prod.keys`.
- nsz defaults: solid for NSP (`-C`), block 1 MiB (`-B`) for XCI, level
  18 (`-l 18`). rom-converto matches: solid for NSP, block for XCI, zstd
  level 18.
- After each compress, the harness feeds the rom-converto output through
  `nsz -D` and compares the SHA-256 of the result to the source.
- The XCI input was staged in a local scratch directory before the run
  so drive speed does not skew the numbers.
- Harness: `rom-converto-benchmark switch --nsp <nsp> --xci <xci>`.

Inputs:
- A 308 MB NSP wrapping a single title, decoded from a solid-mode NSZ.
- A second NSP of 3.3 GB, used only for the large-input decompress row.
- A 6.2 GB XCI with gamecard partitions, decoded from a block-mode
  XCZ.

## Results

| Operation | nsz warm mean | rom-converto warm mean | Delta | Size delta |
|---|---:|---:|---:|---:|
| NSP compress (L18 solid) | 31.365 s (sigma = 1.690) | **18.326 s (sigma = 0.998)** | **0.58x (1.71x faster)** | **-24,042,107 B (-16.5588 %)** |
| NSP decompress (solid, 3.3 GB) | 11.489 s (sigma = 0.306) | **5.418 s (sigma = 0.234)** | **0.47x (2.12x faster)** | - |
| NSP decompress (solid, 308 MB) | 1.555 s (sigma = 0.053) | **0.545 s (sigma = 0.040)** | **0.35x (2.85x faster)** | - |
| NSP verify | - | **0.527 s (sigma = 0.093)** | - | - |
| XCI compress (L18 block) | 167.913 s (sigma = 31.805) | **99.524 s (sigma = 20.448)** | **0.59x (1.69x faster)** | -1,604 B (-0.0000 %) |
| XCI decompress (block) | 24.946 s (sigma = 0.938) | **7.360 s (sigma = 0.372)** | **0.30x (3.39x faster)** | - |
| XCI verify | - | **8.549 s (sigma = 0.567)** | - | - |

The cross-tool round trip passed on all inputs: the rom-converto
compressed file decompresses through `nsz -D` to bytes whose SHA-256
matches the source.

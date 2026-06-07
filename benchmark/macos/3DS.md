# 3DS Z3DS benchmark (macOS, Apple Silicon)

rom-converto on decrypted 3DS ROMs, Z3DS container. These numbers are
specific to the macOS Apple Silicon host described below. The Windows
head-to-head against `z3ds_compressor` lives in [`../3DS.md`](../3DS.md).

## Reference tool availability

`z3ds_compressor` is not distributed as a macOS Apple Silicon binary. The
upstream tool ships Windows and Linux builds only, with no macOS or arm64
release. A head-to-head could not be run on this host, so the table below
is the rom-converto-only output of the harness. The Windows ratios
against the external tool in the parent document still stand. The
decompress rows in that document already have no external column, because
the external tool is compress only.

## Host

- Apple M4, 10 cores, 16 GB unified memory.
- macOS 26.3 (build 25D125), arm64.
- rom-converto built with `cargo build --release`.

## Methodology

- **N = 5** runs per metric, **warm stats exclude run 1** (cold cache).
- A residual process kill runs before every invocation, 3 s cooldown
  after each.
- Both inputs are decrypted with `rom-converto ctr decrypt` first.
  Compression on encrypted 3DS content has near zero ratios, so the
  decrypted form is the fair input.
- Harness: `rom-converto-benchmark ctr --three-ds <3ds> --cia <cia> --rom-converto-only`.

Inputs: a 1.0 GB decrypted cart dump (NCSD format) and a 656 MB decrypted
CIA (CTR package format). The Output column is the size of the produced
compressed file.

## Results

| Operation | rom-converto warm mean | Output |
|---|---:|---:|
| .3ds compress | **0.347 s (sigma = 0.013)** | 779 MB |
| .3ds decompress | **0.451 s (sigma = 0.013)** | - |
| .cia compress | **0.415 s (sigma = 0.007)** | 515 MB |
| .cia decompress | **0.378 s (sigma = 0.015)** | - |

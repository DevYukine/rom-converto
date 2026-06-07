# Wii RVZ benchmark (macOS, Apple Silicon)

rom-converto on Wii disc images, default 128 KiB chunk size, zstd level 5
and level 22. These numbers are specific to the macOS Apple Silicon host
described below. The Windows head-to-head against `DolphinTool` lives in
[`../Wii.md`](../Wii.md).

## Reference tool availability

DolphinTool is not distributed as a macOS Apple Silicon binary. The
Homebrew cask installs only the Dolphin GUI app, which does not include
the command line tool, and the project does not publish a standalone
DolphinTool build for macOS. A head-to-head could not be run on this
host, so the table below is the rom-converto-only output of the harness.
The Windows ratios against DolphinTool in the parent document still
stand.

## Host

- Apple M4, 10 cores, 16 GB unified memory.
- macOS 26.3 (build 25D125), arm64. AES runs through the ARMv8 crypto
  extensions.
- rom-converto built with `cargo build --release`.

## Methodology

- **N = 7** runs per metric, **warm stats exclude run 1** (cold cache).
- A residual process kill runs before every invocation, 3 s cooldown
  after each.
- Compress runs at zstd level 5 and level 22, 128 KiB chunk size.
  Decompress reads an existing RVZ and writes a raw ISO.
- Harness: `rom-converto-benchmark wii --iso <iso> --rom-converto-only`.

Input: one 4.4 GB Wii disc image. The Output column is the size of the
produced file.

## Results

| Operation | rom-converto warm mean | Output |
|---|---:|---:|
| Compress L5 | **3.708 s (sigma = 0.011)** | 2.0 GB |
| Compress L22 | **20.742 s (sigma = 0.018)** | 2.0 GB |
| Decompress | **9.315 s (sigma = 0.230)** | - |

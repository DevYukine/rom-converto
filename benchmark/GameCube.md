# GameCube RVZ benchmark

rom-converto versus `DolphinTool` on a GameCube disc image, default
128 KiB chunk size, zstd level 5 and level 22. Full uninterpreted
tables, hosts, and methodology:

- [Windows, x64](windows/GameCube.md), DolphinTool 2632, head-to-head
- [macOS, Apple Silicon](macos/GameCube.md), rom-converto only
  (DolphinTool ships no macOS arm64 CLI build)

## Summary

| Operation | Windows vs Dolphin | Windows rom-converto | macOS rom-converto |
|---|---:|---:|---:|
| Compress L5 | 1.11x (Dolphin faster) | 1.234 s | 0.512 s |
| Compress L22 | 1.24x faster | 9.193 s | 8.731 s |
| Decompress | 1.79x faster | 1.096 s | 1.193 s |

## Interpretation

- **Decompress** is 1.79x faster than Dolphin and **compress L22** is
  1.24x faster, both with marginally smaller output (about -0.002 %).
- **Compress L5** remains the one loss (1.11x, roughly 125 ms absolute).
  L5 finishes in about a second, so fixed dispatcher overhead shows up
  as a double digit percentage; the bottleneck is sequential reads on
  the dispatcher thread.
- The M4 laptop beats the 5900X desktop on L5 (0.51 s versus 1.23 s)
  where single-thread speed and unified memory dominate, while L22 and
  decompress land close on both hosts.
- `DolphinTool header` accepts every rom-converto RVZ on Windows.

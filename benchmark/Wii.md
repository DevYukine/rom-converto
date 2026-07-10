# Wii RVZ benchmark

rom-converto versus `DolphinTool` on a Wii disc image with encrypted
partitions, default 128 KiB chunk size, zstd level 5 and level 22. Full
uninterpreted tables, hosts, and methodology:

- [Windows, x64](windows/Wii.md), DolphinTool 2632, head-to-head
- [macOS, Apple Silicon](macos/Wii.md), rom-converto only (DolphinTool
  ships no macOS arm64 CLI build)

## Summary

| Operation | Windows vs Dolphin | Windows rom-converto | macOS rom-converto |
|---|---:|---:|---:|
| Compress L5 | 1.18x (Dolphin faster) | 4.959 s | 3.708 s |
| Compress L22 | 1.02x faster | 32.715 s | 20.742 s |
| Decompress | 1.02x faster | 7.573 s | 9.315 s |

## Interpretation

- On Windows every Wii operation lands within 18 % of Dolphin, with
  L22 and decompress at effective parity (1.02x faster). The Wii path
  does more work per cluster than GameCube (SHA-1, AES, hash hierarchy
  recompute), which amortises the fixed dispatcher overhead that hurts
  the GameCube L5 case.
- Compression ratio is at parity on both levels (size deltas within
  +-0.002 %).
- The M4 is markedly faster than the 5900X on L5 and L22 compress on
  this per-core-bound path, while the desktop wins decompress.
- `DolphinTool header` accepts every rom-converto RVZ on Windows.

# 3DS Z3DS benchmark

rom-converto versus `z3ds_compressor` (Azahar Z3DS CLI) on decrypted
3DS ROMs, Z3DS container. Both tools implement the same Azahar Z3DS
format. Full uninterpreted tables, hosts, and methodology:

- [Windows, x64](windows/3DS.md), head-to-head
- [macOS, Apple Silicon](macos/3DS.md), rom-converto only (the external
  tool ships no macOS arm64 build)

## Summary

| Operation | Windows vs external | Windows rom-converto | macOS rom-converto |
|---|---:|---:|---:|
| .3ds compress | 3.53x faster | 0.736 s | 0.347 s |
| .3ds decompress | - (external is compress only) | 0.912 s | 0.451 s |
| .cia compress | 2.50x faster | 0.737 s | 0.415 s |
| .cia decompress | - (external is compress only) | 0.693 s | 0.378 s |

## Interpretation

- **Compress** is 2.5x to 3.5x faster than the external tool on
  Windows, and the `.cia` output is 4.9 % smaller at the same time.
  The `.3ds` output is effectively tied on size (-0.003 %).
- **Decompress** has no external column because `z3ds_compressor` is
  compress only; rom-converto handles both directions in under a
  second on either host.
- Cross-tool parity holds in both directions: the external tool's
  output decompresses through `rom-converto ctr decompress` to bytes
  whose SHA-256 matches the source on both inputs.
- Both hosts benchmark pre-decrypted inputs, since compression on
  encrypted 3DS content has near zero ratios.

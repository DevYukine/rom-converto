# CHD benchmark

rom-converto versus `chdman` on a CD image, default codecs on both
sides. Full uninterpreted tables, hosts, and methodology:

- [Windows, x64](windows/CHD.md), chdman 0.284
- [macOS, Apple Silicon](macos/CHD.md), chdman 0.288

## Summary

rom-converto is faster than chdman on every CHD operation on both
hosts, with the gap widest on the read paths:

| Operation | Windows (Ryzen 9 5900X) | macOS (Apple M4) |
|---|---:|---:|
| CD compress | 1.65x faster | 1.30x faster |
| CD extract | 17.24x faster | 6.39x faster |
| CD verify | 24.22x faster | 7.10x faster |

## Interpretation

- **Compress** wins by 1.3x to 1.65x while producing byte-equivalent
  output: +0 B on Windows, -4 B on macOS (a shorter track-metadata
  string; the compressed hunk stream is identical). Compression ratio
  is effectively the same as chdman's on both hosts.
- **Extract and verify** are an order of magnitude faster. chdman
  re-reads and re-hashes the full image through its generic hunk layer,
  where rom-converto streams sequentially; the higher thread count on
  the Windows host stretches the gap further (17x to 24x versus 6x to
  7x on the 10-core M4).
- `chdman info` accepts every rom-converto output on both hosts, and
  extract output matches the source byte for byte.

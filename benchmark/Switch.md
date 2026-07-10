# Switch NSZ benchmark

rom-converto versus `nsz` (https://github.com/nicoboss/nsz) on NSP and
XCI inputs. Both tools produce zstd-compressed `.ncz` files inside a
PFS0 (NSZ) or HFS0 (XCZ) outer container. Full uninterpreted tables,
hosts, and methodology:

- [Windows, x64](windows/Switch.md), nsz 4.6.1
- [macOS, Apple Silicon](macos/Switch.md)

## Summary

Delta is the rom-converto / nsz ratio; below 1.00x means rom-converto
is faster.

| Operation | Windows (Ryzen 9 5900X) | macOS (Apple M4) |
|---|---:|---:|
| NSP compress (L18 solid) | 0.58x (1.71x faster) | 0.72x (1.40x faster) |
| NSP decompress (large input) | 0.47x (2.12x faster) | 0.74x (1.35x faster) |
| NSP decompress (308 MB) | 0.35x (2.85x faster) | 1.07x |
| XCI compress (L18 block) | 0.59x (1.69x faster) | 0.80x (1.25x faster) |
| XCI decompress (block) | 0.30x (3.39x faster) | 0.93x (1.07x faster) |

## Interpretation

- **NSP compress** wins on both hosts and produces 16.6 % smaller
  output on the shared 308 MB input, from
  `EnableLongDistanceMatching = true` being on by default (nsz hides
  the same option behind an off-by-default `-L` flag).
- **Decompress** gains grow with core count: the 24-thread desktop
  reaches 2.1x to 3.4x where the 10-core M4 sits at 1.0x to 1.35x.
  Block-mode XCZ decompress parallelises per block and benefits the
  most.
- The cross-tool round trip (rom-converto compress, then `nsz -D`,
  then SHA-256 against the source) passed for every input on both
  hosts. XCI compressed sizes match nsz's to within framing noise
  (-1,604 B on Windows, -990 B on macOS).

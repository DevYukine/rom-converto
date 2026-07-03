# Formats

rom-converto reads a per-platform input and writes a matching output format. This page
explains what each output format is, which term applies to each platform's input, and where
each format works.

## Terminology

The input vocabulary is per platform on purpose. These are the exact terms used everywhere in
the tool and docs.

| Platform (code) | Input term | Input formats | Output format | Operation |
|---|---|---|---|---|
| Nintendo 3DS (`ctr`) | ROM | `.3ds`, `.cci`, `.cxi`, `.cia`; CDN content | Z3DS | compress / decompress |
| GameCube (`dol`) | disc image | `.iso`, `.gcm`, `.gcz`, NKit | RVZ | compress / migrate / decompress |
| Wii (`rvl`) | disc image | `.iso`, `.wbfs`, `.wia`, `.gcz`, NKit | RVZ | compress / migrate / decompress |
| Wii U (`wup`) | title (NUS or loadiine); disc image for `.wud`/`.wux` | NUS, loadiine, `.wud`, `.wux` | WUA | compress; decrypt |
| Switch (`nx`) | container | NSP, XCI | NSZ, XCZ | compress / decompress |
| CD / DVD (`chd`) | disc image | `.cue`+`.bin`, `.iso` | CHD | compress / extract |
| PSP / PS2 (`cso`) | ISO | `.iso` | CSO, ZSO | compress / decompress |
| CD (`cue`) | disc image | `.cue`+`.bin` | merged `.bin`/`.cue` | merge |

Restoring a CHD is always called "extract", because it recreates sidecar files (`.bin`+`.cue`
for CD, `.iso` for DVD). Every other format uses "decompress" for the reverse operation. The
collective phrase for all inputs is "ROMs and disc images".

## What each format is

- **Z3DS** wraps a decrypted 3DS ROM in seekable zstd. Extensions are `.zcia`, `.zcci`,
  `.zcxi`, and `.z3dsx`. Compression only works on decrypted ROMs, since encrypted 3DS content
  has a near-zero compression ratio.
- **RVZ** is Dolphin's compressed disc format for GameCube and Wii, with per-block compression
  and a partition-aware hash pipeline for Wii.
- **GCZ, WIA, and NKit** are older compressed disc containers that rom-converto reads as
  GameCube and Wii inputs. `dol migrate` and `rvl migrate` convert them to RVZ after an
  integrity check, and `compress` migrates a legacy container automatically when it is given
  one. GCZ (`.gcz`) and NKit (`.nkit.iso`, `.nkit.gcz`) are accepted on both consoles; WIA
  (`.wia`) is Wii only.
- **WUA** is Cemu's Wii U archive format. One archive can bundle a base title, update, and DLC,
  each under its own `<titleId>_v<version>/` folder.
- **NSZ / XCZ** compress a Switch NSP or XCI with zstd inside the NCZ format, in solid mode
  (one frame per NCA) or block mode (fixed-size frames for random reads).
- **CHD** is MAME's compressed hunk format. CD-mode CHDs come from `.bin`+`.cue` pairs or
  MODE1/2048 ISOs; DVD-mode CHDs come from PS2-DVD and PSP ISOs. The CD/DVD media type is
  probed from the image.
- **CSO / ZSO** compress a PSP or PS2 ISO block by block. CSO (CISO v1) targets PSP hardware
  with CFW and PPSSPP; ZSO (LZ4) targets PS2 hardware via Open PS2 Loader.
- **CUE/BIN** merging combines a multi-bin `.cue` (one `.bin` per track) into a single
  `.bin` + `.cue` pair for emulators that cannot load split images.

## Output compatibility

Every output format matches the established encoder for its platform, so a file verifies
against that tool and loads in the same players. RVZ and NSZ/XCZ are byte-identical to the
reference encoder at matching settings; CSO/ZSO and CHD are format-compatible with maxcso and
chdman.

| Format | Reference tool | Notes |
|---|---|---|
| RVZ | Dolphin | Byte-identical in both directions on GameCube and Wii, including RVZ migrated from a GCZ, WIA, or NKit source |
| NSZ / XCZ | nsz | Byte-identical to `nsz` and `nsz -D`; matching CLI defaults |
| CSO / ZSO | maxcso | maxcso-compatible defaults, including index shift and store-raw fallback |
| CHD | chdman | `createcd` and `createdvd` equivalents; also reads chdman DVD CHDs with `huff` and `flac` hunks |

The Wii U `.wua` pipeline has no comparable reference CLI (Cemu ships the format but not a
standalone compressor), so no head-to-head numbers are published for it. See the
[benchmark files](../README.md#benchmarks) for measured performance.

## Where each format works

| Target | Recommended format |
|---|---|
| PCSX2 / NetherSX2 (PS2 emulators) | DVD-mode CHD (default codecs) |
| PPSSPP (PSP emulator) | CHD or CSO |
| Real PSP with CFW | CSO |
| Real PS2 via Open PS2 Loader | ZSO |
| GameCube / Wii emulation (Dolphin) | RVZ |
| Switch emulation | NSZ / XCZ |
| Wii U emulation (Cemu) | WUA |
| 3DS emulation (Azahar) | decrypted ROM, or Z3DS for storage |

DVD-mode CHDs use compatibility-first codecs (`lzma` + `zlib`) that load everywhere, including
AetherSX2 and NetherSX2. The opt-in `--zstd` flag adds a better ratio for modern players; some
older players and cores do not support zstd-compressed CHD. PSP images get 2048-byte hunks
automatically, which is what PPSSPP expects.

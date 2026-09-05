# Formats

rom-converto converts selected ROM and disc-image formats. It also inspects a wider
set of files with `info`. Use `rom-converto capabilities` for the exact operations
and `info` extensions in the installed build.

## Conversion formats

| Family | Input | Output | Main operations |
|---|---|---|---|
| Nintendo 3DS (`ctr`) | `.cia`, `.3ds`/`.cci`, `.cxi`, `.3dsx`; CDN content | Z3DS: `.zcia`, `.zcci`, `.zcxi`, `.z3dsx`; encrypted/decrypted ROMs; CIA/CCI | compress, decompress, encrypt, decrypt, CIA/CCI conversion, CDN to CIA |
| GameCube (`dol`) | `.iso`, `.gcm`; legacy `.gcz`, `.nkit.iso`, `.nkit.gcz` | `.rvz`, then `.iso` on decompress | compress, migrate, decompress |
| Wii (`rvl`) | `.iso`, `.wbfs`; legacy `.gcz`, `.wia`, NKit | `.rvz`, then `.iso` or `.wbfs` | compress, migrate, decompress |
| Wii U (`wup`) | NUS or loadiine title directory, `.wud`, `.wux` | `.wua` | compress, decrypt NUS to loadiine |
| Switch (`nx`) | `.nsp`, `.xci` | `.nsz`, `.xcz`, merged NSP/XCI, or per-title NSPs | compress, decompress, merge, split |
| CHD (`chd`) | `.cue` with tracks, suitable `.iso`, LaserDisc `.avi`, or legacy CHD v1 to v4 | CHD v5 | compress, migrate, extract, convert DVD CHD to CSO/ZSO |
| CSO (`cso`) | `.iso` | `.cso` or `.zso` | compress, decompress, convert to CHD |
| CUE (`cue`) | Multi-file `.cue`/`.bin` | single `.cue`/`.bin`, `.iso`, CSO/ZSO | merge, to-iso, to-cso |
| Original Xbox (`xbox`) | Full XDVDFS `.iso` or extracted game directory | `.xiso` | convert, extract |
| Xbox 360 (`xenon`) | XDVDFS `.iso`; extracted game directories for ZAR only | `.zar` or GoD install tree | compress, extract, convert |
| PlayStation 3 (`ps3`) | Encrypted disc `.iso` | Plain `.iso` | decrypt |
| Nintendo DS (`nds`) | `.nds`, `.dsi` | Same extension | encrypt or decrypt the secure area |
| PSP (`psp`) | `EBOOT.PBP` or PSN `.pkg` | extracted files or `.iso` | extract, to-iso |
| PS Vita (`vita`) | `.pkg` | extracted files | extract |

`.dax` is a legacy, decode-only input for CSO commands. It cannot be created.
CHD extraction recreates `.bin` plus `.cue` for CD media and an `.iso` for DVD
media, so its reverse operation is named `extract`.

## Format notes

### Z3DS

Z3DS uses seekable zstd around a 3DS ROM. By default, `ctr compress` rejects an
encrypted input. Decrypt first, or pass `--allow-encrypted` when that tradeoff is
intentional. `ctr decompress`, `ctr encrypt`, and `ctr decrypt` use the matching ROM
extension. `ctr convert` changes `.cia` and `.3ds`/`.cci`; its CIA output is unsigned
and intended for CFW or emulators, not a stock 3DS.

### RVZ and legacy Nintendo disc containers

RVZ is the GameCube and Wii output container. `dol migrate` accepts GCZ and NKit.
`rvl migrate` also accepts WIA. Migration checks the legacy container before writing
RVZ. An RVZ decompresses to ISO for GameCube; Wii writes WBFS only when the requested
output name ends in `.wbfs`.

### WUA

WUA is the Wii U archive used by Cemu. A single archive can contain base, update, and
DLC title inputs. A WUA is not a general Wii U disc-image replacement: use `wup compress`
only with the accepted title layouts or a `.wud`/`.wux` disc image.

### NSZ and XCZ

`nx compress` maps NSP to NSZ and XCI to XCZ. The command needs `prod.keys` to process
the NCAs. `solid` stores one zstd frame per NCA. `block` stores independent frames and
uses `block_size_exp` to set the block size.

`nx merge` combines uncompressed NSP/XCI containers; `nx split` writes per-title NSPs.
Merge selects the highest content versions and drops unselected files, so splitting
is not a lossless reversal. Selected NCA bytes are preserved, but generated XCI headers
are unsigned. The tool warns that merged output is intended for emulator use.

### CHD

CHD mode is chosen from the input: CUE input makes a CD CHD, suitable ISO input is
probed as CD or DVD, and `.avi` selects LaserDisc. `--cd`, `--dvd`, and `--ld` override
the automatic choice where the command permits it. LaserDisc input must use uncompressed
YUY2, UYVY, or VYUY video and 8- or 16-bit PCM audio. LaserDisc CHDs can be written but
are not extracted by this tool.

`info` reads CHD versions 1 through 5. `chd migrate` upgrades supported v1 to v4
files to v5 while preserving decoded data and updating legacy metadata. It writes
`<name>.v5.chd` by default; `--in-place` replaces the source. Parent-dependent images
are unsupported, and legacy audio/video images may grow substantially. See the
[CLI reference](cli.md#chd-cd--dvd--laserdisc) for options and limits.

Stored legacy hashes are shown by `info`, but migration does not validate them.
`extract`, `verify`, and `to-cso` require v5, so migrate older files first.

### CSO and ZSO

CSO is a CISO v1 container. ZSO uses LZ4 blocks. Choose the output with
`cso compress --format cso` or `--format zso`. These containers are block-compressed
ISO storage; use the target software's documentation to choose a format it supports.

### XISO, ZAR, and GoD

XISO contains an original Xbox XDVDFS game partition. Converting a full disc image
trims it to that content. ZAR is the Xbox 360 ZArchive written by `xenon compress`.
Both commands also accept an extracted game directory.

`xenon convert` writes an Xbox 360 disc ISO as Games on Demand (GoD): a header at
`<TITLEID>/00007000/<MEDIAID>` and parts under `<MEDIAID>.data/DataNNNN`. It trims
unused disc data and adds hashes; it does not compress the data. The container is
unsigned and intended for modified consoles. This command requires a disc image
with a root `default.xex`, not an extracted game folder.

## Recommended formats

Choose a format for the emulator or loader you use. These recommendations cover
rom-converto's compression and disc-conversion targets. Sources checked September 5, 2026.

| Console / media | Recommended format | Target and limits |
|---|---|---|
| Nintendo 3DS | Z3DS (`.zcci`) | [Azahar 2123+](https://github.com/azahar-emu/azahar/releases/tag/2123), using decrypted ROMs. Compressed CIA packages (`.zcia`) are installed instead. |
| GameCube / Wii | RVZ | [Dolphin](https://github.com/dolphin-emu/dolphin/blob/master/Readme.md). For a real Wii with [USB Loader GX](https://github.com/wiidev/usbloadergx/blob/enhanced/source/usbloader/wbfs/wbfs_fat.cpp), decompress to WBFS or ISO. |
| Wii U | WUA | [Cemu](https://github.com/cemu-project/Cemu/blob/main/src/Cafe/TitleList/TitleList.cpp). Can bundle the base game, updates, and DLC. |
| Switch | NSP / XCI for playback | [Eden](https://github.com/eden-emulator/mirror/blob/master/src/core/loader/loader.cpp) loads NSP/XCI. [NSZ / XCZ](https://github.com/nicoboss/nsz/blob/master/docs/usage.md) are compressed storage formats; use `nx decompress` first. |
| PlayStation | CHD | [DuckStation](https://github.com/stenzek/duckstation/blob/master/README.md). Convert from CUE/BIN and retain any required SBI file for LibCrypt games. |
| PlayStation 2 | CHD for emulation; ZSO for hardware | [PCSX2](https://github.com/PCSX2/pcsx2/blob/master/pcsx2/VMManager.cpp) reads CHD. Use ZSO with [Open PS2 Loader](https://github.com/ps2homebrew/Open-PS2-Loader/blob/master/README.md) on a real PS2. |
| PSP | CSO | [PPSSPP and real PSPs with custom firmware](https://www.ppsspp.org/docs/getting-started/dumping-games/). PPSSPP also reads DVD-mode CHD, but CSO works across both targets. |
| Saturn | CHD | [Beetle Saturn](https://docs.libretro.com/library/beetle_saturn/). Convert from the CUE sheet to retain the track layout. |
| Xbox | XISO (`.iso`) | [xemu](https://github.com/xemu-project/xemu-website/blob/master/docs/docs/disc-images.md) requires the game-partition image, not a full Redump ISO. XISO trims data; it does not compress it. |
| Xbox 360 | ZAR | Loads directly in [Xenia Canary](https://github.com/xenia-canary/xenia-canary/blob/canary_experimental/src/xenia/emulator.cc). Upstream Xenia does not support ZAR. |
| LaserDisc | CHD | [MAME](https://docs.mamedev.org/tools/chdman.html), using LaserDisc mode with AVHUFF and a supported AVI input. |

DS, PS3, and Vita operations do not produce compressed formats. Dreamcast GDI is
inspection-only input; rom-converto cannot convert it to CHD.

## Inspection support

`info` reads metadata from all conversion formats plus PS1/PS2/PSP disc images,
Dreamcast GDI, and cartridge images for NES, SNES, Nintendo 64, Game Boy and Game Boy
Color, Game Boy Advance, Mega Drive/Genesis, 32X, Master System, Game Gear, Virtual Boy,
WonderSwan, Neo Geo Pocket, Lynx, Atari 7800, and FDS. It identifies shared `.iso`,
`.rvz`, `.gcz`, `.wia`, `.cue`, and `.ngc` extensions from file content where needed.
An extension alone is not proof that a file is a valid image.

## Examples

```text
rom-converto dol compress game.gcm game.rvz
rom-converto rvl decompress game.rvz game.wbfs
rom-converto nx compress --keys prod.keys game.nsp
rom-converto chd compress game.cue game.chd
rom-converto xbox convert ./extracted-game game.xiso
```

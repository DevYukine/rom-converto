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
| Wii U (`wup`) | NUS or loadiine title directory, `.wud`, `.wux` | `.wua`; `.wux` or `.wud` | compress, to-wux, to-wud, decrypt NUS to loadiine |
| Switch (`nx`) | `.nsp`, `.xci` | `.nsz`, `.xcz`, merged NSP/XCI, or per-title NSPs | compress, decompress, merge, split |
| CHD (`chd`) | `.cue` with tracks, suitable `.iso`, LaserDisc `.avi`, or legacy CHD v1 to v4 | CHD v5 | compress, migrate, extract, convert DVD CHD to CSO/ZSO |
| CSO (`cso`) | `.iso` | `.cso` or `.zso` | compress, decompress, convert to CHD |
| CUE (`cue`) | Multi-file `.cue`/`.bin` | single `.cue`/`.bin`, `.iso`, CSO/ZSO | merge, to-iso, to-cso |
| Original Xbox (`xbox`) | Full XDVDFS `.iso` or extracted game directory | `.xiso` | convert, extract |
| Xbox 360 (`xenon`) | XDVDFS `.iso`; extracted game directories for ZAR only | `.zar` or GoD install tree | compress, extract, convert |
| PlayStation 3 (`ps3`) | Encrypted disc `.iso` | Plain `.iso` | decrypt |
| Nintendo DS (`ntr`) | `.nds`, `.dsi` | Same extension | encrypt or decrypt the secure area |
| PSP (`psp`) | `EBOOT.PBP` or PSN `.pkg` | extracted files or `.iso` | extract, to-iso |
| PS Vita (`vita`) | `.pkg` | extracted files | extract |

`.dax` is a legacy, decode-only input for CSO commands. It cannot be created.
CHD extraction recreates `.bin` plus `.cue` for CD media and an `.iso` for DVD
media, so its reverse operation is named `extract`.

## Large files and memory

Payload memory does not grow with the size of the input file. Every operation
reads and writes in fixed-size blocks, and `info` reads only headers and the
metadata it reports. Block index tables (CHD maps, CSO indexes, RVZ and WIA group
tables) are kept in memory and take a few bytes per block.

| Behavior | Detail |
|---|---|
| Demand decoding | `info` on Z3DS, NCZ, RVZ, WIA, GCZ, CHD, CSO, ZSO, DAX, ZAR, WUA and XEX decodes only the frames, blocks or hunks that hold the requested bytes. |
| WUX dedup table | `wup to-wux` keeps a dedup entry per unique 32 KiB sector, tens of MiB for a full disc. Its sector hashers share the 512 MiB worker working set described below. |
| Worker memory | Decoders whose unit size comes from the file (CHD, CSO, Z3DS, RVZ, NCZ blocks) and the ZAR, Z3DS and NCZ block compressors size their worker pools against a 512 MiB working set per operation, counting codec contexts, queued units and the writer queue. Default unit sizes keep full parallelism; only user-chosen unit sizes far above the defaults (for example NCZ blocks of 256 MiB) shrink the pool. A unit that does not fit the working set on its own is never rejected: it is decoded on one worker with at most two units live (one decoding, one being written), and formats whose codec streams (Z3DS frames, plain and packed RVZ chunks, NCZ) stream it in 4 MiB pieces instead of holding it whole. The legacy GCZ, WIA and NKit readers used by `migrate` and `verify` are capped at one worker per core and about 128 MiB of groups in flight (`in_flight_cap`), so large blocks or chunks reduce the worker count; their memory is the file's block or chunk size times the worker count plus, for WIA LZMA files, the dictionary per worker (files declaring a dictionary above 256 MiB are rejected). |
| Size checks | Sizes and counts declared inside a file are checked against the file before anything is allocated, so metadata memory scales with what is actually stored on disk, not with a declared value. For RVZ this rejects chunk sizes that are neither a power of two of at least 32 KiB nor a multiple of 2 MiB, any group with stored bytes past the end of the file, raw regions that under-declare the groups their span needs, partition data entries whose second entry does not continue the first on a chunk boundary, group and raw-data tables whose stored size exceeds zstd's worst-case expansion for the declared entry count, table streams that decode past the declared entry count, groups whose stored bytes or declared packed record stream exceed the chunk's worst-case size, stored packed groups whose size differs from their declared record stream, and ISO sizes above 64 GiB. A partitioned RVZ whose chunk size exceeds 2 MiB is refused by `info` and conversion and reported as unverifiable by `verify`. `info` retains only the entries it reports (for example PKG artwork and SFO items, CHD metadata tags without their payloads). |
| Sheets | CUE and GDI sheets are parsed line by line; a single line is kept up to 16 MiB, far beyond any directive, so a bogus multi-gigabyte sheet never loads whole. CUE FILE names and GDI track file names must stay inside the sheet's folder: absolute paths, `..` that climbs above it, a `\` in a name on Linux and macOS, and a `:` on Windows are refused. |
| Organize and patch | Per-unit digests (the patch CRC pass and DAT matching) stream in 4 MiB chunks; torrentzip and patch application stream in fixed chunks; an existing `.m3u` playlist is compared under a 16 MiB cap: anything larger warns and counts as differing. |
| Validation scope | `info` validates only the ranges it reads. `verify` checks structure and stored hashes; with `--full` (RVZ, CSO, Z3DS) it decodes every byte. Full conversions always decode every byte. |
| Hash cache | The persistent hash cache keeps up to 250,000 entries and evicts entries least recently written or used in a run that stores the cache. A cache file that decodes to more than 256 MiB is ignored and rebuilt. |

WIA groups and CHD hunks are held whole because the format compresses each as one
stream; RVZ chunks, Z3DS frames and NCZ blocks stream in 4 MiB pieces (see above).
Solid NSZ and XCZ compression uses zstd's multithreaded mode with one job per core:
its memory depends on the level and the core count, not on the input size. The
desktop app can run up to eight operations at once, each with its own budget.

## Format notes

### PS4 and PS5 PKG files

PS4 and PS5 `.pkg` files are inspect-only. `info` reads the header, entry table, `param.sfo` or `param.json`, and the icon and background art, none of which is encrypted, so no keys are needed. Extraction is not offered: the file system inside these packages is encrypted with per-package keys that the tool does not embed.

### Z3DS

Z3DS uses seekable zstd around a 3DS ROM. By default, `ctr compress` rejects an
encrypted input. Decrypt first, or pass `--allow-encrypted` when that tradeoff is
intentional. `ctr decompress`, `ctr encrypt`, and `ctr decrypt` use the matching ROM
extension. `ctr convert` changes `.cia` and `.3ds`/`.cci`; its CIA output is unsigned
and intended for CFW or emulators, not a stock 3DS. Its `.3ds` output is padded to the
next cartridge size like a real cart dump; pass `--trim` to end it after the last
partition instead. A trimmed file still carries the full card size in its header, so
`ctr info` reports the padded size rather than the file size. Z3DS files end with a
seek table; `ctr info` reads only the frames it needs through that table and rejects a
payload without one.

### RVZ and legacy Nintendo disc containers

RVZ is the GameCube and Wii output container. `dol migrate` accepts GCZ and NKit.
`rvl migrate` also accepts WIA. Migration checks the legacy container before writing
RVZ. An RVZ decompresses to ISO for GameCube; Wii writes WBFS only when the requested
output name ends in `.wbfs`.

Chunk sizes above 2 MiB are only unsupported for Wii partition data:
the container-level rule accepts a power of two of at least 32 KiB or a
multiple of 2 MiB, and raw-only (GameCube) containers written that way
decode like any other container; a chunk is streamed in bounded windows
only when it exceeds the decoder working set or the disc reader's
whole-chunk limit, and is decoded whole otherwise,
while partitioned containers walk one 2 MiB cluster of sectors per
chunk, so a larger chunk is reported as unverifiable by `verify` and
refused by conversion instead of being treated as corrupt. Partition
data entries must
also be split on a chunk boundary: when a partition carries two data entries,
the first entry's byte span has to be a whole number of chunks, or the
second entry's groups would not start on a chunk boundary. The decoder
rejects such containers, and this writer always splits there.

### WUA

WUA is the Wii U archive used by Cemu. A single archive can contain base, update, and
DLC title inputs. A WUA is not a general Wii U disc-image replacement: use `wup compress`
only with the accepted title layouts or a `.wud`/`.wux` disc image.

### WUX

WUX is a lossless container for Wii U disc images. It stores each 32 KiB sector once and
points repeated sectors at the first copy. `wup to-wud` restores a plain `.wud`, and
WUX to WUD to WUX reproduces the original file. A split `.wud` set must be complete:
`wup to-wux` reads its `game_part1.wud` part and skips the continuation parts.

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

## Organize targets

`rom-converto organize` picks these recommendations for you: it sorts a library
folder into per-console folders and converts each file to the best archival format
for its console. The per-console targets and behavior are described under
[organize](cli.md#organize).

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

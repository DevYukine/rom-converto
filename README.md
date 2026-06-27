# rom-converto

A utility suite for converting, compressing, encrypting, and decrypting ROMs across **Nintendo 3DS**, **GameCube**, **Wii**, **Wii U**, **Nintendo Switch**, and **CD image** formats.

Available as both a **command line tool** and a **desktop GUI application**.

Built for developers, tinkerers and archivists.

## Features

### Nintendo 3DS (CTR)

* [x] Convert CDN files to `.cia`
* [x] Generate tickets for CDN files
* [x] Decrypt `.cia`, `.3ds`, `.cci`, and `.cxi` for emulator use (e.g. [Azahar](https://azahar-emu.org/))
* [x] Compress and decompress ROMs using the Z3DS format (seekable zstd)
* [x] Verify `.cia` legitimacy and `.3ds` / `.cci` NCCH partition integrity
* [x] See [`benchmark/3DS.md`](benchmark/3DS.md) for performance numbers

### GameCube (DOL)

* [x] Compress `.iso` / `.gcm` to Dolphin's `.rvz` format
* [x] Decompress `.rvz` back to raw `.iso`
* [x] Byte identical output against Dolphin's own encoder and decoder
* [x] See [`benchmark/GameCube.md`](benchmark/GameCube.md) for performance numbers

### Wii (RVL)

* [x] Compress `.iso` / `.wbfs` to Dolphin's `.rvz` format
* [x] Decompress `.rvz` back to raw `.iso`
* [x] Full partition pipeline: AES-CBC sector encryption, H0/H1/H2 hash hierarchy, per-chunk exception list, partial-cluster partitions
* [x] Byte identical output against Dolphin's own encoder and decoder
* [x] See [`benchmark/Wii.md`](benchmark/Wii.md) for performance numbers

### Wii U (WUP)

* [x] Bundle titles, updates, and DLC into Cemu's `.wua` archive format
* [x] Decrypt NUS-format titles (`title.tmd` + `title.tik` + `.app` files) into a loadiine-shaped `meta/code/content` tree
* [x] Also accepts the community layout variant (`tmd.<N>` + optional `cetk.<N>` + extensionless content files)
* [x] Wii U disc images (`.wud` / `.wux`) accepted as `.wua` input, with per-disc key files
* [x] FST-aware inherited-file skipping so update overlays install cleanly on top of the base title

### Nintendo Switch (NX)

* [x] Compress `.nsp` to `.nsz` and `.xci` to `.xcz` using zstd inside the NCZ format
* [x] Decompress `.nsz` / `.xcz` back to raw `.nsp` / `.xci`
* [x] Both solid mode (one zstd frame per NCA) and block mode (random-read friendly fixed-size frames)
* [x] Verify per-NCA hash integrity on any container (`.nsp` / `.nsz` / `.xci` / `.xcz`)
* [x] Drop-in replacement for [`nsz`](https://github.com/nicoboss/nsz) with byte-identical output and matching CLI defaults
* [x] See [`benchmark/Switch.md`](benchmark/Switch.md) for performance numbers

### CD / DVD images (CHD / CUE+BIN)

* [x] Compress `.bin` + `.cue` pairs to CD-mode `.chd`
* [x] Compress CD-media `.iso` (PS1, PS2-CD) to CD-mode `.chd` with a single MODE1/2048 track (the `chdman createcd` equivalent)
* [x] Compress PS2-DVD / PSP `.iso` to DVD-mode `.chd` (the `chdman createdvd` equivalent); the CD/DVD media type is probed from the image, so the createcd vs createdvd mixup cannot happen
* [x] Compatibility-first DVD codecs (`lzma` + `zlib`) that load everywhere including AetherSX2/NetherSX2; opt-in `--zstd` for modern emulators
* [x] PSP images get 2048-byte hunks automatically (what PPSSPP expects); PS2 images use the chdman default
* [x] Extract `.chd` back to `.bin` + `.cue` (CD) or `.iso` (DVD), auto-detected
* [x] Verify `.chd` integrity via SHA-1 checksums, with optional header repair
* [x] Read chdman-produced DVD CHDs including `huff` and `flac` compressed hunks
* [x] Merge a multi-bin `.cue` (one `.bin` per track) into a single `.bin` + `.cue` pair, for emulators that cannot load split images
* [x] See [`benchmark/CHD.md`](benchmark/CHD.md) for performance numbers

### PSP / PS2 compressed ISOs (CSO / ZSO)

* [x] Compress `.iso` to `.cso` (CISO v1) for real PSP hardware with CFW and for PPSSPP
* [x] Compress `.iso` to `.zso` (LZ4) for real PS2 hardware via Open PS2 Loader 1.2+ and ARK-4 on PSP
* [x] Decompress `.cso` / `.zso` back to the original `.iso`
* [x] Verify container structure, with an optional full decode pass
* [x] maxcso-compatible defaults: 2 KiB blocks (16 KiB for 2 GiB+ inputs), automatic index shift for large images, per-block store-raw fallback

Where each format works:

| Target | Recommended format |
|---|---|
| PCSX2 / NetherSX2 (PS2 emulators) | DVD-mode CHD (default codecs) |
| PPSSPP (PSP emulator) | CHD or CSO |
| Real PSP with CFW | CSO |
| Real PS2 via Open PS2 Loader | ZSO |

### Application

* [x] Command line interface with progress bars and a post-run space-saved summary
* [x] Desktop GUI with drag and drop batch processing
* [x] Self update from GitHub releases (CLI)

## GUI Application

The desktop app provides a visual interface for all operations. Built with [Tauri](https://tauri.app/), [Nuxt](https://nuxt.com/) and [Tailwind CSS](https://tailwindcss.com/).

**Highlights:**

* Drag and drop files directly into the application
* Batch process multiple files at once (drop several files to queue them up)
* Real time progress tracking for all operations
* State persists when switching between pages
* Works on Windows, macOS and Linux

### Running the GUI in Development

1. Install [Rust 1.88+](https://www.rust-lang.org/tools/install) and [Node.js 22+](https://nodejs.org/)
2. Install [pnpm](https://pnpm.io/installation)
3. Clone the repository
4. Install frontend dependencies:
   ```
   cd crates/rom-converto-gui
   pnpm install
   ```
5. Start the development server:
   ```
   pnpm tauri dev
   ```

## CLI Commands

All commands that write an output file use `--on-conflict` to decide what happens when the output already exists. The choices are `error` (the default, refuse and stop), `overwrite` (replace the existing output), `skip` (leave the existing output and move on, reported as skipped in the summary), and `rename` (write to the next free numbered sibling, for example `Game.chd` becomes `Game (1).chd`). `-f`/`--force` is a shorthand for `--on-conflict overwrite` and cannot be combined with `--on-conflict`. For `wup decrypt` the output is a directory, so `rename` is not supported there and falls back to `error`. For `chd extract` and `cue merge`, which write more than one file, the policy applies to the base output path and the sidecars follow it.

After `compress`, `decompress`, and `convert` operations, the tool prints a closing summary of bytes processed and space saved or expanded, for example `12 files: 12.4 GiB -> 4.1 GiB, saved 8.3 GiB (67%) in 2m14s`. Verify and extract operations print a file count and elapsed time instead.

The `compress`, `decompress`, and `chd extract` commands also accept `--report <FILE>` to write a structured run report after the run. The format is inferred from the file extension and the numbers match the closing summary. The report file is overwritten directly and does not go through `--on-conflict`, since it is an output you named explicitly rather than a converted ROM. See [Run reports](#run-reports) for the formats and schema.

### CTR (Nintendo 3DS)

| Command | Description |
|---|---|
| `ctr cdn-to-cia <CDN_DIR> [OUTPUT]` | Convert a CDN directory to `.cia` |
| `ctr generate-cdn-ticket <CDN_DIR> [OUTPUT]` | Generate a `.tik` ticket from CDN content |
| `ctr decrypt <INPUT> [OUTPUT]` | Decrypt an encrypted ROM for emulator use |
| `ctr compress <INPUT> [OUTPUT]` | Compress a decrypted ROM to Z3DS format |
| `ctr decompress <INPUT> [OUTPUT]` | Decompress a Z3DS file back to the original ROM |
| `ctr convert <INPUT> [OUTPUT]` | Convert between `.cia` and `.cci`/`.3ds`, direction auto-detected |
| `ctr verify <INPUT>` | Verify `.cia` legitimacy or `.3ds`/`.cci` NCCH integrity |
| `ctr info <INPUT>` | Inspect 3DS metadata; see the Info section |

**`cdn-to-cia` flags:**

| Flag | Description |
|---|---|
| `-C, --cleanup` | Remove original CDN files after conversion |
| `-R, --recursive` | Convert each immediate child directory of CDN_DIR to a `.cia`. This scans the direct subdirectories, unlike the file-recursive subcommands below, so it takes no `--max-depth` |
| `-T, --ensure-ticket-exists` | Auto-generate a ticket file if one is not found |
| `-D, --decrypt` | Also decrypt the CIA after creation |
| `-Z, --compress` | Also compress the CIA after creation (implies decrypt) |
| `--on-conflict <POLICY>` | What to do when the output exists: `error` (default), `overwrite`, `skip`, or `rename` |
| `-f, --force` | Alias for `--on-conflict overwrite` |

**`decrypt` / `compress` / `decompress` / `convert` flags:**

| Flag | Description |
|---|---|
| `-o, --output <FILE>` | Output path (alternative to the positional OUTPUT argument) |
| `-R, --recursive` | Recursively process every matching file in INPUT and its subdirectories |
| `--max-depth <N>` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |
| `--on-conflict <POLICY>` | What to do when the output exists: `error` (default), `overwrite`, `skip`, or `rename` |
| `-f, --force` | Alias for `--on-conflict overwrite` |

**`verify` flags:**

| Flag | Description |
|---|---|
| `--full` | Also verify content hashes against the TMD (CIA only, slower). `--verify-content` is a visible alias for this flag |
| `-R, --recursive` | Recursively verify every matching file in INPUT and its subdirectories and print a summary |
| `--max-depth <N>` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |

> **`generate-cdn-ticket`:** Generated tickets use placeholder values (null Console ID, etc.) and only work on modded consoles and emulators. They will not work on stock hardware.

> **`decrypt`:** Supports `.cia`, `.3ds`, `.cci` and `.cxi` files. The format is detected automatically. Place a `seeddb.bin` file next to the executable to resolve seeds locally. If none is found, the tool will fetch the required seed from Nintendo's API.

> **`compress` / `decompress`:** Supported input formats for compression: `.cia`, `.cci`, `.3ds`, `.cxi`, `.3dsx`. Output files use the Z3DS format (`.zcia`, `.zcci`, `.zcxi`, `.z3dsx`). Compression only works on decrypted ROMs, since encrypted ROMs have near-zero compression ratios. The output file path defaults to the input path with the extension updated automatically.

> **`convert`:** Direction is auto-detected from the INPUT extension: `.cia` becomes `.3ds` (CCI/NCSD), and `.3ds`/`.cci` become `.cia`. CIA output is unsigned with a zero title key; compatible with CFW (Luma3DS) and emulators, but not installable on stock 3DS.

> **`verify`:** Exits nonzero on verification failure.

---

### DOL (GameCube)

| Command | Description |
|---|---|
| `dol compress <INPUT> [OUTPUT]` | Compress a `.iso`/`.gcm` to Dolphin's `.rvz` |
| `dol decompress <INPUT> [OUTPUT]` | Decompress a `.rvz` back to `.iso` |
| `dol verify <INPUT>` | Verify RVZ container hashes, or compute a whole-disc SHA-1 with `--full` |
| `dol info <INPUT>` | Inspect GameCube disc metadata; see the Info section |

**Flags:**

| Flag | Applies to | Description |
|---|---|---|
| `-l, --level <LEVEL>` | `compress` | Zstandard compression level (signed, defaults to 22, Dolphin's max non-extreme) |
| `--chunk-size <BYTES>` | `compress` | Chunk size in bytes, power of two between 32 KiB and 2 MiB (defaults to 128 KiB to match Dolphin) |
| `-o, --output <FILE>` | `compress`, `decompress` | Output path (alternative to the positional OUTPUT argument) |
| `--on-conflict <POLICY>` | `compress`, `decompress` | What to do when the output exists: `error` (default), `overwrite`, `skip`, or `rename` |
| `-f, --force` | `compress`, `decompress` | Alias for `--on-conflict overwrite` |
| `-R, --recursive` | `compress`, `decompress`, `verify` | Recursively process every matching file in INPUT and its subdirectories |
| `--max-depth <N>` | `compress`, `decompress`, `verify` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |
| `--report <FILE>` | `compress`, `decompress` | Write a run report to FILE. Format inferred from the extension: `.csv`, `.json`, `.html` or `.htm`. Unknown extensions default to JSON. Covers every processed file with paths, status, byte sizes, space-saved ratio, elapsed time, and any error. Overwritten directly, ignoring `--on-conflict` |
| `--full` | `verify` | Decode the whole disc and compute a whole-disc SHA-1 |

---

### RVL (Wii)

| Command | Description |
|---|---|
| `rvl compress <INPUT> [OUTPUT]` | Compress a `.iso`/`.wbfs` to Dolphin's `.rvz` |
| `rvl decompress <INPUT> [OUTPUT]` | Decompress a `.rvz` back to `.iso` |
| `rvl verify <INPUT>` | Verify RVZ container hashes, or recompute the Wii partition hash tree with `--full` |
| `rvl info <INPUT>` | Inspect Wii disc metadata; see the Info section |

Flags match the `dol` commands. `--full` on `rvl verify` decrypts every partition cluster and recomputes the H0/H1/H2 hash tree.

> `dol` and `rvl` share one RVZ pipeline. Output files are byte identical to Dolphin's own encoder and decoder in both directions, on both GameCube and Wii. See the [Benchmarks](#benchmarks) section for measured performance.

---

### WUP (Wii U)

| Command | Description |
|---|---|
| `wup compress -o <OUTPUT> <INPUTS>...` | Bundle one or more titles into a Cemu `.wua` archive |
| `wup decrypt -o <OUTPUT> <INPUT>` | Decrypt a NUS directory into a loadiine `meta/code/content` tree |
| `wup verify <INPUT>` | Verify Wii U content SHA-1 against the TMD |
| `wup info <PATH>` | Inspect Wii U title metadata; see the Info section |

**`compress` flags:**

| Flag | Description |
|---|---|
| `-o, --output <FILE>` | Output `.wua` file path |
| `-l, --level <LEVEL>` | Zstd compression level 0..=22 (0 = Cemu default of 6) |
| `--key <KEYFILE>` | Disc master key file. Pass once per disc input in positional order |
| `--on-conflict <POLICY>` | What to do when the output exists: `error` (default), `overwrite`, `skip`, or `rename` |
| `-f, --force` | Alias for `--on-conflict overwrite` |

**`decrypt` flags:**

| Flag | Description |
|---|---|
| `-o, --output <DIR>` | Output directory |
| `--on-conflict <POLICY>` | What to do when the output directory exists: `error` (default), `overwrite`, or `skip`; `rename` is not supported for directory outputs and falls back to `error` |
| `-f, --force` | Alias for `--on-conflict overwrite` |

**`verify` flags:**

| Flag | Description |
|---|---|
| `--key <KEYFILE>` | Disc master key file (`.wud`/`.wux` inputs only) |
| `-R, --recursive` | Recursively verify every `.wud` / `.wux` disc image in INPUT and its subdirectories. NUS title directories are detected among the immediate children of INPUT only |
| `--max-depth <N>` | Limit disc image recursion depth with `--recursive`. `1` = top level only. Default: unlimited. Does not affect NUS title directory discovery |

> **`compress`:** Each input is auto-detected as a loadiine directory, a NUS directory, or a disc image (`.wud` / `.wux`). Disc images need a 16-byte master key; keys are resolved in order from `--key` flags, a sibling `<disc>.key` file, or `game.key` in the same directory. Multiple titles (base + update + DLC) can be bundled into one archive and each lands under its own `<titleId>_v<version>/` folder, the layout Cemu expects.

> **`decrypt`:** Writes the decrypted tree to the output directory. Handles both the canonical Nintendo layout (`title.tmd` + `title.tik` + `{id}.app`) and the community layout variant (`tmd.<N>` + optional `cetk.<N>` + extensionless content files). When no ticket is present, the title key is derived from the title id via the Nintendo CDN's PBKDF2 scheme.

---

### NX (Nintendo Switch)

| Command | Description |
|---|---|
| `nx compress <INPUT> [-o OUTPUT]` | Compress a `.nsp` to `.nsz` or a `.xci` to `.xcz` |
| `nx decompress <INPUT> [-o OUTPUT]` | Decompress a `.nsz` / `.xcz` back to `.nsp` / `.xci` |
| `nx verify <INPUT>` | Verify per-NCA hash integrity of any Switch container |
| `nx info <INPUT>` | Inspect Switch container metadata; see the Info section |

**`compress` flags:**

| Flag | Description |
|---|---|
| `--keys <PRODKEYS>` | Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` (or `%USERPROFILE%/.switch/prod.keys` on Windows) |
| `-o, --output <FILE>` | Output path. Defaults to the input with the extension switched (`.nsp` -> `.nsz`, `.xci` -> `.xcz`) |
| `-l, --level <LEVEL>` | Zstd compression level 1..=22 (defaults to 18, matching `nsz`) |
| `--mode <MODE>` | `solid` (one zstd frame per NCA, default for NSP) or `block` (independent zstd frames per fixed-size block, default for XCI) |
| `--block-size-exp <EXP>` | Block-mode block size as `1 << exp` bytes, range 14..=32 (defaults to 20 = 1 MiB, matching `nsz`) |
| `--on-conflict <POLICY>` | What to do when the output exists: `error` (default), `overwrite`, `skip`, or `rename` |
| `-f, --force` | Alias for `--on-conflict overwrite` |
| `-R, --recursive` | Compress every `.nsp` and `.xci` found in INPUT and its subdirectories |
| `--max-depth <N>` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |
| `--report <FILE>` | Write a run report to FILE. Format inferred from the extension: `.csv`, `.json`, `.html` or `.htm`. Unknown extensions default to JSON. Overwritten directly, ignoring `--on-conflict` |

**`decompress` flags:**

| Flag | Description |
|---|---|
| `--keys <PRODKEYS>` | Path to `prod.keys`. Same default as `compress` |
| `-o, --output <FILE>` | Output path. Defaults to the input with the extension switched (`.nsz` -> `.nsp`, `.xcz` -> `.xci`) |
| `--on-conflict <POLICY>` | What to do when the output exists: `error` (default), `overwrite`, `skip`, or `rename` |
| `-f, --force` | Alias for `--on-conflict overwrite` |
| `-R, --recursive` | Decompress every `.nsz` and `.xcz` found in INPUT and its subdirectories |
| `--max-depth <N>` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |
| `--report <FILE>` | Write a run report to FILE. Format inferred from the extension: `.csv`, `.json`, `.html` or `.htm`. Unknown extensions default to JSON. Overwritten directly, ignoring `--on-conflict` |

**`verify` flags:**

| Flag | Description |
|---|---|
| `--keys <PRODKEYS>` | Path to `prod.keys`. Same default as `compress` |
| `-R, --recursive` | Verify every `.nsp`, `.nsz`, `.xci`, and `.xcz` found in INPUT and its subdirectories |
| `--max-depth <N>` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |

> **`compress` / `decompress`:** Outputs are byte identical to `nsz` / `nsz -D` at matching settings. `prod.keys` is required to derive the per-NCA section keys; the file is read but never modified. Tickets inside the container are kept as-is so installation on console still works.

> **`verify`:** Walks every NCA inside the container and checks the stored hash hierarchy (FS hashes for PFS0 sections, IVFC for RomFS sections). Works on already-compressed `.nsz` / `.xcz` without decompressing first. Exits nonzero on verification failure.

---

### CHD

| Command | Description |
|---|---|
| `chd compress <INPUT> [OUTPUT]` | Compress a `.cue` or `.iso` to `.chd`; CD vs DVD media is auto-detected (PS1/PS2-CD iso becomes CD-mode, PS2-DVD/PSP iso becomes DVD-mode) |
| `chd extract <INPUT> [OUTPUT]` | Extract a `.chd` file back to `.bin` + `.cue` (CD) or `.iso` (DVD) |
| `chd verify <INPUT>` | Verify the SHA1 integrity of a `.chd` file |
| `chd info <INPUT>` | Inspect CHD metadata; see the Info section |

**Flags:**

| Flag | Applies to | Description |
|---|---|---|
| `--on-conflict <POLICY>` | `compress`, `extract` | What to do when the output exists: `error` (default), `overwrite`, `skip`, or `rename` |
| `-f, --force` | `compress`, `extract` | Alias for `--on-conflict overwrite` |
| `--dvd` / `--cd` | `compress` | Override the auto-detected mode (CD mode needs a cue sheet) |
| `--hunk-size <BYTES>` | `compress` | DVD hunk size, a multiple of 2048; defaults to 4096, or 2048 for detected PSP images |
| `--zstd` | `compress` | Add zstd to the DVD codec set; better ratio, but rejected by AetherSX2/NetherSX2 |
| `-o, --output <FILE>` | `compress`, `extract` | Output path (alternative to the positional OUTPUT argument) |
| `-R, --recursive` | `compress`, `extract`, `verify` | Recursively process every matching file in INPUT and its subdirectories |
| `--max-depth <N>` | `compress`, `extract`, `verify` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |
| `--report <FILE>` | `compress`, `extract` | Write a run report to FILE. Format inferred from the extension: `.csv`, `.json`, `.html` or `.htm`. Unknown extensions default to JSON. Overwritten directly, ignoring `--on-conflict`. Extract rows carry zero byte sizes since extraction writes several files |
| `-p, --parent <PARENT>` | `extract`, `verify` | Specify a parent CHD for parent-child relationships |
| `--fix` | `verify` | Correct SHA1 values in the CHD header if mismatches are found |

---

### CSO / ZSO

| Command | Description |
|---|---|
| `cso compress <INPUT> [OUTPUT]` | Compress an `.iso` to `.cso` (default) or `.zso` |
| `cso decompress <INPUT> [OUTPUT]` | Restore the original `.iso` from a `.cso` / `.zso` |
| `cso verify <INPUT>` | Validate the container index; `--full` decodes every block |
| `cso info <INPUT>` | Inspect CSO/ZSO metadata; see the Info section |

**Flags:**

| Flag | Applies to | Description |
|---|---|---|
| `--format <cso\|zso>` | `compress` | Output container: CSO for PSP/PPSSPP, ZSO for PS2 via OPL |
| `--block-size <BYTES>` | `compress` | Block size, a power of two; defaults to 2048 (16384 for 2 GiB+ inputs) |
| `-o, --output <FILE>` | `compress`, `decompress` | Output path (alternative to the positional OUTPUT argument) |
| `--on-conflict <POLICY>` | `compress`, `decompress` | What to do when the output exists: `error` (default), `overwrite`, `skip`, or `rename` |
| `-f, --force` | `compress`, `decompress` | Alias for `--on-conflict overwrite` |
| `-R, --recursive` | `compress`, `decompress`, `verify` | Recursively process every matching file in INPUT and its subdirectories |
| `--max-depth <N>` | `compress`, `decompress`, `verify` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |
| `--report <FILE>` | `compress`, `decompress` | Write a run report to FILE. Format inferred from the extension: `.csv`, `.json`, `.html` or `.htm`. Unknown extensions default to JSON. Overwritten directly, ignoring `--on-conflict` |
| `--full` | `verify` | Decode every block instead of only checking the index |

---

### CUE/BIN

| Command | Description |
|---|---|
| `cue merge <INPUT_CUE> <OUTPUT_CUE>` | Merge a multi-bin `.cue` into a single `.bin` + `.cue` pair (the merged `.bin` is named after the output `.cue`) |

**Flags:**

| Flag | Applies to | Description |
|---|---|---|
| `--on-conflict <POLICY>` | `merge` | What to do when an output exists: `error` (default), `overwrite`, `skip`, or `rename`; the `.bin` sidecar follows the renamed `.cue` |
| `-f, --force` | `merge` | Alias for `--on-conflict overwrite` |

---

### Info

```
rom-converto <console> info <INPUT> [--json] [--save-icon DIR] [--keys FILE]
```

Inspect a ROM file or title directory and print the embedded metadata: title, version, region, content layout, age ratings, and the embedded icon (where the format carries one). Maker / company codes are resolved to the publisher name (table ported from Dolphin). Encrypted 3DS CIA inputs are decrypted on the fly so the NCCH header fields show real values, not garbage. No decryption files are written to disk. Add `--json` for a machine-readable payload (the Tauri GUI uses the same JSON shape).

| Subcommand | Coverage |
|---|---|
| `ctr info <FILE>` | CIA / NCSD / NCCH; SMDH multilingual titles, region lock, age ratings, 48x48 icon. Encrypted CIA is auto-decrypted to read the NCCH header. |
| `dol info <FILE>` | GameCube `.iso`, `.gcm`, or `.rvz`; boot.bin header, BNR1/BNR2 banner with 96x32 image, publisher name. |
| `rvl info <FILE>` | Wii `.iso`, `.rvz`, or `.wbfs`; disc header, partition layout, TMD (title id, IOS), IMET banner names, 192x64 banner image from `opening.bnr` (falls back to the icon). |
| `wup info <PATH>` | loadiine + NUS directories, `.wua` archives, and `.wud`/`.wux` disc images; TMD + meta.xml with multilingual names, region, age ratings, save sizes, GamePad requirement, supported accessories, mastering date, decoded `iconTex.tga` icon. Pass `--keys` with the disc master key file when reading a `.wud`/`.wux`. |
| `nx info <FILE>` | NSP / NSZ / XCI / XCZ; container listing, tickets, CNMT, NACP, JPEG icon. Reports compression status (NSP vs NSZ), distribution (digital vs cartridge), structure classifier (scene / converted / CDN / homebrew), base title id for patches and DLC, decoded language list, age ratings per rating board. Full info needs `--keys prod.keys`, partial info without. |
| `chd info <FILE>` | CHD v5; version, codecs, hunk geometry, SHA-1 triplet, per-track CHT2 metadata, VERS / DVD tags |
| `cso info <FILE>` | CSO / ZSO; format and version, block geometry, index shift, raw block count, compression ratio |

`--save-icon DIR` writes the embedded icon as `<title_id>.png` into `DIR`. Supported by `ctr`, `dol`, `rvl`, `nx`, and `wup` info; not supported by `chd` or `cso` (those formats carry no embedded artwork). `--keys` applies to `nx info` (path to `prod.keys`) and to `wup info` on `.wud`/`.wux` disc images (disc master key file). Other consoles ignore it and will return an error if it is passed.

Format notes:

- `.rvz` (Wii and GameCube) and `.wbfs` (Wii) are read directly. Only the disc areas that are actually needed get decompressed, so no temp files are written and memory stays at a few MB.
- `.wua` (Wii U Cemu archive) is read directly. When an archive bundles base + update + DLC, the base title is shown, the bundled titles are listed, and the version includes the update.
- `.cso` / `.zso` are read directly.
- WIA, GCZ, NFS, and TGC are not supported.

---

### Shell completions

```
rom-converto shell-completions <SHELL> [--out-dir DIR]
```

Generates a tab-completion script for the rom-converto CLI. Writes to stdout by default. Pass `--out-dir DIR` to write the canonical per-shell filename inside `DIR` and print the resulting path.

| Shell | Install one-liner |
|---|---|
| Bash | `rom-converto shell-completions bash > ~/.local/share/bash-completion/completions/rom-converto` |
| Zsh | `rom-converto shell-completions zsh > "${fpath[1]}/_rom-converto"` |
| Fish | `rom-converto shell-completions fish > ~/.config/fish/completions/rom-converto.fish` |
| PowerShell | `rom-converto shell-completions powershell >> $PROFILE` |
| Elvish | `rom-converto shell-completions elvish > ~/.elvish/lib/rom-converto.elv` |

---

### Self-Update

```
rom-converto self-update
```

Checks GitHub for a newer release and replaces the current binary in place.

## Run Reports

Pass `--report <FILE>` to `compress`, `decompress`, or `chd extract` to write a structured report of the run. The format is chosen from the file extension: `.csv` writes CSV, `.json` writes JSON, `.html` and `.htm` write a self-contained HTML table. Any other extension, or no extension, writes JSON. The report file is always created and overwritten directly; it does not go through `--on-conflict`. The numbers match the closing summary line.

The CTR (3DS) commands and all `verify` commands are not yet covered.

The columns are stable and appear in this order: `input_path`, `output_path`, `operation`, `status`, `input_bytes`, `output_bytes`, `ratio_pct`, `elapsed_ms`, `error`. `status` is `ok`, `skipped`, or `failed`. `input_bytes` and `output_bytes` are raw byte counts. `ratio_pct` is the space saved as a percent: positive when compression shrank the file, negative when decompression expanded it (honest expansion), and empty or `null` for skipped, failed, or zero-input rows. `output_path` and `error` are empty for skipped and failed rows.

The CSV file has a header row and one row per file, with RFC 4180 quoting for any field that contains a comma, double quote, or newline. It has no totals row.

The JSON file is an object with a `files` array and a `totals` object. Each entry in `files` has the fields above, with `ratio_pct` and `error` serialized as `null` when absent. The `totals` object has `total_files`, `ok`, `skipped`, `failed`, `total_input_bytes`, `total_output_bytes`, and `elapsed_ms`. An empty run writes `{"files":[],"totals":{...}}` with every total set to zero.

The HTML file is a single self-contained page with no external assets: one table with a header, one row per file (sizes shown human readable), and a totals row in the table footer.

## Benchmarks

RVZ, CHD, and Switch operations are measured against
`DolphinTool.exe` 2603a-x64, `chdman.exe` 0.284, and `nsz`
respectively, each tool on its own default settings, N = 10
interleaved warm runs. Full methodology and per-run detail live
alongside the results:

* [`benchmark/3DS.md`](benchmark/3DS.md): 3DS ROM results (Z3DS)
* [`benchmark/GameCube.md`](benchmark/GameCube.md): GameCube disc image results (RVZ)
* [`benchmark/Wii.md`](benchmark/Wii.md): Wii disc image results (RVZ)
* [`benchmark/Switch.md`](benchmark/Switch.md): Switch NSP/XCI results (NSZ/XCZ)
* [`benchmark/CHD.md`](benchmark/CHD.md): CD image results (CHD)

The Wii U `.wua` pipeline has no comparable reference CLI (Cemu ships the format but not a standalone compressor), so no head-to-head numbers are published for it.

## Project Structure

The project is organized as a Cargo workspace with three crates:

| Crate | Description |
|---|---|
| `rom-converto-lib` | Core library with all conversion, compression and decryption logic |
| `rom-converto-cli` | Command line interface |
| `rom-converto-gui` | Desktop GUI application (Tauri + Nuxt) |

## Development

### Prerequisites

1. Install [Rust 1.88+](https://www.rust-lang.org/tools/install)
2. For the GUI: Install [Node.js 22+](https://nodejs.org/) and [pnpm](https://pnpm.io/installation)

### Running the CLI in Development

```
cargo run -p rom-converto-cli
```

### Building Release Binaries

```
cargo build --release -p rom-converto-cli
```

The resulting binary will be at `target/release/rom-converto`.

### Cross-Tool Parity Tests

Some tests cross-check output against the reference tools when they
are available. They are skipped unless the environment variable
points at the binary:

```
ROMCONVERTO_CHDMAN=$(which chdman) cargo test -p rom-converto-lib chdman
ROMCONVERTO_MAXCSO=$(which maxcso) cargo test -p rom-converto-lib maxcso
```

## Built With

* [Rust](https://www.rust-lang.org/) and [tokio](https://tokio.rs/) for the core logic and async runtime
* [Tauri](https://tauri.app/) and [Nuxt](https://nuxt.com/) for the desktop GUI
* [clap](https://github.com/clap-rs/clap) for command line argument parsing
* [binrw](https://github.com/jam1garner/binrw) for reading/writing binary data structures
* [RustCrypto](https://github.com/rustcrypto) for AES encryption, key derivation, and hashing
* [zstd](https://github.com/gyscos/zstd-rs) for seekable zstd compression (Z3DS format)
* [flate2](https://github.com/rust-lang/flate2-rs) / [lzma-sdk-sys](https://github.com/nicowillis/lzma-sdk-sys) / [flacenc](https://github.com/yotarok/flacenc-rs) for CHD compression codecs
* [indicatif](https://github.com/console-rs/indicatif) for CLI progress bars
* [Pinia](https://pinia.vuejs.org/) for GUI state management
* [Tailwind CSS](https://tailwindcss.com/) for GUI styling

## Contributing

Contributions are welcome! Please open an issue or PR if you'd like to help shape this project.

## Versioning

We use [SemVer](http://semver.org/) for versioning. See
the [tags on this repository](https://github.com/DevYukine/rom-converto/tags) for available versions.

## Authors

* **DevYukine** - *Initial work* - [DevYukine](https://github.com/DevYukine)

See also the list of [contributors](https://github.com/DevYukine/rom-converto/contributors) who participated in this
project.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Acknowledgments

These projects/resources were extremely helpful during development:

* [Makerom/Ctrtool](https://github.com/3DSGuy/Project_CTR)
* [Cia-Unix](https://github.com/shijimasoft/cia-unix)
* [ctrdecrypt](https://github.com/shijimasoft/ctrdecrypt)
* [make_cdn_cia](https://github.com/llakssz/make_cdn_cia)
* [TikGenerator](https://github.com/matiffeder/TikGenerator)
* [3DSBrew](https://www.3dbrew.org/wiki/Main_Page)
* [decrypt.py](https://gist.github.com/melvincabatuan/3675deef7c58ce13b28236e61917e577)

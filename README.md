# rom-converto

A utility suite for converting, compressing, encrypting, and decrypting ROM formats across **Nintendo 3DS**, **GameCube**, **Wii**, **Wii U**, **Nintendo Switch**, and **CD image** formats.

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

### CD images (CHD / CUE+BIN)

* [x] Compress `.bin` + `.cue` pairs to `.chd`
* [x] Extract `.chd` back to `.bin` + `.cue`
* [x] Verify `.chd` integrity via SHA-1 checksums, with optional header repair
* [x] Merge a multi-bin `.cue` (one `.bin` per track) into a single `.bin` + `.cue` pair, for emulators that cannot load split images
* [x] See [`benchmark/CHD.md`](benchmark/CHD.md) for performance numbers

### Application

* [x] Command line interface with progress bars
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

### CTR (Nintendo 3DS)

| Command | Description |
|---|---|
| `ctr cdn-to-cia <CDN_DIR> [OUTPUT]` | Convert a CDN directory to `.cia` |
| `ctr generate-cdn-ticket <CDN_DIR> [OUTPUT]` | Generate a `.tik` ticket from CDN content |
| `ctr decrypt <INPUT> <OUTPUT>` | Decrypt an encrypted ROM for emulator use |
| `ctr compress <INPUT> [OUTPUT]` | Compress a decrypted ROM to Z3DS format |
| `ctr decompress <INPUT> [OUTPUT]` | Decompress a Z3DS file back to the original ROM |

**`cdn-to-cia` flags:**

| Flag | Description |
|---|---|
| `-C, --cleanup` | Remove original CDN files after conversion |
| `-R, --recursive` | Process all subdirectories, converting each to `.cia` |
| `-T, --ensure-ticket-exists` | Auto-generate a ticket file if one is not found |
| `-D, --decrypt` | Also decrypt the CIA after creation |
| `-Z, --compress` | Also compress the CIA after creation (implies decrypt) |

> **`generate-cdn-ticket`:** Generated tickets use placeholder values (null Console ID, etc.) and only work on modded consoles and emulators. They will not work on stock hardware.

> **`decrypt`:** Supports `.cia`, `.3ds`, `.cci` and `.cxi` files. The format is detected automatically. Place a `seeddb.bin` file next to the executable to resolve seeds locally. If none is found, the tool will fetch the required seed from Nintendo's API.

> **`compress` / `decompress`:** Supported input formats for compression: `.cia`, `.cci`, `.3ds`, `.cxi`, `.3dsx`. Output files use the Z3DS format (`.zcia`, `.zcci`, `.zcxi`, `.z3dsx`). Compression only works on decrypted ROMs, since encrypted ROMs have near-zero compression ratios. The output file path defaults to the input path with the extension updated automatically.

---

### DOL (GameCube)

| Command | Description |
|---|---|
| `dol compress <INPUT> [OUTPUT]` | Compress a `.iso`/`.gcm` to Dolphin's `.rvz` |
| `dol decompress <INPUT> [OUTPUT]` | Decompress a `.rvz` back to `.iso` |

**Flags:**

| Flag | Applies to | Description |
|---|---|---|
| `-l, --level <LEVEL>` | `compress` | Zstandard compression level (signed, defaults to 22, Dolphin's max non-extreme) |
| `--chunk-size <BYTES>` | `compress` | Chunk size in bytes, power of two between 32 KiB and 2 MiB (defaults to 128 KiB to match Dolphin) |

---

### RVL (Wii)

| Command | Description |
|---|---|
| `rvl compress <INPUT> [OUTPUT]` | Compress a `.iso`/`.wbfs` to Dolphin's `.rvz` |
| `rvl decompress <INPUT> [OUTPUT]` | Decompress a `.rvz` back to `.iso` |

Flags match the `dol` commands.

> `dol` and `rvl` share one RVZ pipeline. Output files are byte identical to Dolphin's own encoder and decoder in both directions, on both GameCube and Wii. See the [Benchmarks](#benchmarks) section for measured performance.

---

### WUP (Wii U)

| Command | Description |
|---|---|
| `wup compress -o <OUTPUT> <INPUTS>...` | Bundle one or more titles into a Cemu `.wua` archive |
| `wup decrypt -o <OUTPUT> <INPUT>` | Decrypt a NUS directory into a loadiine `meta/code/content` tree |

**`compress` flags:**

| Flag | Description |
|---|---|
| `-o, --output <FILE>` | Output `.wua` file path |
| `-l, --level <LEVEL>` | Zstd compression level 0..=22 (0 = Cemu default of 6) |
| `--key <KEYFILE>` | Disc master key file. Pass once per disc input in positional order |

> **`compress`:** Each input is auto-detected as a loadiine directory, a NUS directory, or a disc image (`.wud` / `.wux`). Disc images need a 16-byte master key; keys are resolved in order from `--key` flags, a sibling `<disc>.key` file, or `game.key` in the same directory. Multiple titles (base + update + DLC) can be bundled into one archive and each lands under its own `<titleId>_v<version>/` folder, the layout Cemu expects.

> **`decrypt`:** Writes the decrypted tree to the output directory. Handles both the canonical Nintendo layout (`title.tmd` + `title.tik` + `{id}.app`) and the community layout variant (`tmd.<N>` + optional `cetk.<N>` + extensionless content files). When no ticket is present, the title key is derived from the title id via the Nintendo CDN's PBKDF2 scheme.

---

### NX (Nintendo Switch)

| Command | Description |
|---|---|
| `nx compress <INPUT> [-o OUTPUT]` | Compress a `.nsp` to `.nsz` or a `.xci` to `.xcz` |
| `nx decompress <INPUT> [-o OUTPUT]` | Decompress a `.nsz` / `.xcz` back to `.nsp` / `.xci` |
| `nx verify <INPUT>` | Verify per-NCA hash integrity of any Switch container |

**`compress` flags:**

| Flag | Description |
|---|---|
| `--keys <PRODKEYS>` | Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` (or `%USERPROFILE%/.switch/prod.keys` on Windows) |
| `-o, --output <FILE>` | Output path. Defaults to the input with the extension switched (`.nsp` -> `.nsz`, `.xci` -> `.xcz`) |
| `-l, --level <LEVEL>` | Zstd compression level 1..=22 (defaults to 18, matching `nsz`) |
| `--mode <MODE>` | `solid` (one zstd frame per NCA, default for NSP) or `block` (independent zstd frames per fixed-size block, default for XCI) |
| `--block-size-exp <EXP>` | Block-mode block size as `1 << exp` bytes, range 14..=32 (defaults to 20 = 1 MiB, matching `nsz`) |

> **`compress` / `decompress`:** Outputs are byte identical to `nsz` / `nsz -D` at matching settings. `prod.keys` is required to derive the per-NCA section keys; the file is read but never modified. Tickets inside the container are kept as-is so installation on console still works.

> **`verify`:** Walks every NCA inside the container and checks the stored hash hierarchy (FS hashes for PFS0 sections, IVFC for RomFS sections). Works on already-compressed `.nsz` / `.xcz` without decompressing first.

---

### CHD

| Command | Description |
|---|---|
| `chd compress <INPUT_CUE> <OUTPUT>` | Compress a `.bin` + `.cue` pair to `.chd` |
| `chd extract <INPUT> <OUTPUT>` | Extract a `.chd` file back to `.bin` + `.cue` |
| `chd verify <INPUT>` | Verify the SHA1 integrity of a `.chd` file |

**Flags:**

| Flag | Applies to | Description |
|---|---|---|
| `-f, --force` | `compress` | Overwrite output file if it already exists |
| `-p, --parent <PARENT>` | `extract`, `verify` | Specify a parent CHD for parent-child relationships |
| `--fix` | `verify` | Correct SHA1 values in the CHD header if mismatches are found |

---

### CUE/BIN

| Command | Description |
|---|---|
| `cue merge <INPUT_CUE> <OUTPUT_CUE>` | Merge a multi-bin `.cue` into a single `.bin` + `.cue` pair (the merged `.bin` is named after the output `.cue`) |

**Flags:**

| Flag | Applies to | Description |
|---|---|---|
| `-f, --force` | `merge` | Overwrite output files if they already exist |

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
| `rvl info <FILE>` | Wii `.iso` or `.rvz`; disc header, partition layout, TMD (title id, IOS), IMET banner names, 48x48 channel icon. |
| `wup info <PATH>` | loadiine + NUS directories and `.wua` archives; TMD + meta.xml with multilingual names, region, age ratings, save sizes, GamePad requirement, supported accessories, mastering date. |
| `nx info <FILE>` | NSP / NSZ / XCI / XCZ; container listing, tickets, CNMT, NACP, JPEG icon. Reports compression status (NSP vs NSZ), distribution (digital vs cartridge), structure classifier (scene / converted / CDN / homebrew), base title id for patches and DLC, decoded language list, age ratings per organisation. Full info needs `--keys prod.keys`; degrades gracefully without. |
| `chd info <FILE>` | CHD v5; version, codecs, hunk geometry, SHA-1 triplet, per-track CHT2 metadata, VERS / DVD tags |

`--save-icon DIR` writes the embedded icon as `<title_id>.png` into `DIR` (3DS, GameCube, Wii, and Switch). `--keys` is honoured only by `nx info`.

Format notes:

- `.rvz` for Wii and GameCube is transparently decompressed to a temporary ISO. The temp file is deleted when the command finishes.
- `.wua` (Wii U Cemu archive) is read directly. When an archive bundles a base title plus update and DLC, the first title is shown.
- `.wbfs` is not supported; extract to raw ISO first.
- WIA, CISO, GCZ, NFS, and TGC are not supported.

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

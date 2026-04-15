# rom-converto

A utility suite for converting, compressing, encrypting, and decrypting ROM formats across **Nintendo 3DS**, **GameCube**, **Wii**, and **CD image** formats.

Available as both a **command line tool** and a **desktop GUI application**.

Built for developers, tinkerers and archivists.

## Features

### Nintendo 3DS (CTR)

* [x] Convert CDN files to `.cia`
* [x] Generate tickets for CDN files
* [x] Decrypt `.cia`, `.3ds`, `.cci`, and `.cxi` for emulator use (e.g. [Azahar](https://azahar-emu.org/))
* [x] Compress and decompress ROMs using the Z3DS format (seekable zstd)
* [x] Verify `.cia` legitimacy and `.3ds` / `.cci` NCCH partition integrity

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

### CD images (CHD)

* [x] Compress `.bin` + `.cue` pairs to `.chd`
* [x] Extract `.chd` back to `.bin` + `.cue`
* [x] Verify `.chd` integrity via SHA-1 checksums, with optional header repair
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

### Self-Update

```
rom-converto self-update
```

Checks GitHub for a newer release and replaces the current binary in place.

## Benchmarks

RVZ and CHD operations are measured against `DolphinTool.exe` 2603a-x64
and `chdman.exe` 0.284 respectively, each tool on its own default
settings, N = 10 interleaved warm runs. Full methodology and per-run
detail live alongside the results:

* [`benchmark/GameCube.md`](benchmark/GameCube.md): GameCube disc image results (RVZ)
* [`benchmark/Wii.md`](benchmark/Wii.md): Wii disc image results (RVZ)
* [`benchmark/CHD.md`](benchmark/CHD.md): CD image results (CHD)

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

# rom-converto

[![Test Commit](https://github.com/DevYukine/rom-converto/actions/workflows/tests.yml/badge.svg)](https://github.com/DevYukine/rom-converto/actions/workflows/tests.yml)
[![Latest release](https://img.shields.io/github/v/release/DevYukine/rom-converto)](https://github.com/DevYukine/rom-converto/releases/latest)

rom-converto converts, compresses, verifies, encrypts, and decrypts ROMs and disc images for Nintendo 3DS, GameCube, Wii, Wii U, Nintendo Switch, and CD or DVD media. It ships as a cross-platform command line tool, a desktop GUI, and a Rust library that both front ends call. Its output matches the established encoder for each format, so a rom-converto file drops straight into the emulators and tools people already use.

## Supported formats

| Platform | Input | Output | Compatible with |
|---|---|---|---|
| Nintendo 3DS (`ctr`) | `.3ds`, `.cci`, `.cxi`, `.cia`, CDN content | Z3DS | Azahar |
| GameCube (`dol`) | `.iso`, `.gcm`, `.gcz`, NKit | RVZ | Dolphin |
| Wii (`rvl`) | `.iso`, `.wbfs`, `.wia`, `.gcz`, NKit | RVZ | Dolphin |
| Wii U (`wup`) | NUS or loadiine title, `.wud`, `.wux` | WUA | Cemu |
| Switch (`nx`) | NSP, XCI | NSZ, XCZ | nsz |
| CD / DVD (`chd`) | `.cue`+`.bin`, `.iso` | CHD | chdman, PPSSPP, PCSX2 |
| PSP / PS2 (`cso`) | `.iso` | CSO, ZSO | maxcso, PPSSPP, Open PS2 Loader |
| CD (`cue`) | `.cue`+`.bin` | merged `.bin`/`.cue` | any emulator |

For RVZ and NSZ/XCZ the output is byte-identical to the reference encoder (Dolphin, nsz) at matching settings, so it verifies against that tool and loads in the same players. CSO/ZSO output is maxcso-compatible and CHD output matches chdman's `createcd`/`createdvd`, so both interoperate with their reference tools. See [`docs/formats.md`](docs/formats.md) for what each format is and where it works.

Single-image commands (compress, decompress, convert, extract, verify, info, and `hash`) also read a `.zip`, `.7z`, `.rar`, `.tar`, or `.tar.gz`/`.tgz` archive directly and operate on the first matching member. See [`docs/cli.md`](docs/cli.md) for the details.

## Installation

Download a prebuilt binary from the [GitHub Releases](https://github.com/DevYukine/rom-converto/releases) page. The CLI and GUI are published for Windows, macOS, and Linux.

To build from source you need a recent stable Rust toolchain:

```
cargo build --release -p rom-converto-cli
```

The binary lands at `target/release/rom-converto`. Building the GUI additionally needs Node.js 22 or newer and pnpm; see [`docs/development.md`](docs/development.md).

## Quick start

```
# Compress a GameCube disc image to RVZ
rom-converto dol compress game.iso

# Decompress it back to a raw ISO
rom-converto dol decompress game.rvz

# Decrypt a 3DS ROM for emulator use
rom-converto ctr decrypt game.cia game.decrypted.cia

# Re-encrypt a decrypted 3DS ROM
rom-converto ctr encrypt game.decrypted.cia game.encrypted.cia

# Compress a whole folder of Switch games, previewing first
rom-converto nx compress -R ./switch --dry-run
rom-converto nx compress -R ./switch

# Hash a directory and write a report
rom-converto hash -R ./roms --report hashes.csv
```

Add `-R`/`--recursive` to any conversion to process a directory tree, and `--dry-run` to
preview a run without writing anything. Launch the desktop app with `pnpm tauri dev` from
`crates/rom-converto-gui`, or run a downloaded GUI binary.

## Command line

Each top-level command is a console or format family, and every family has operations such as `compress`, `decompress`, `verify`, and `info`.

| Command | Purpose |
|---|---|
| `ctr` | Convert, decrypt, compress, and verify Nintendo 3DS ROMs |
| `dol` | Compress, migrate, and verify GameCube disc images (RVZ) |
| `rvl` | Compress, migrate, and verify Wii disc images (RVZ) |
| `wup` | Bundle and decrypt Wii U titles (WUA) |
| `nx` | Compress and verify Switch containers (NSZ/XCZ) |
| `chd` | Compress, extract, and verify CD/DVD images (CHD) |
| `cso` | Compress and verify PSP/PS2 ISOs (CSO/ZSO) |
| `cue` | Merge a multi-bin `.cue` into one `.bin`/`.cue` pair |
| `dat` | Identify, verify, and rename ROMs against the Playmatch database |
| `hash` | Compute CRC32, SHA-1, MD5, and SHA-256 digests |
| `playlist` | Generate `.m3u` files for multi-disc sets |
| `shell-completions` | Print a tab-completion script for your shell |
| `self-update` | Replace the binary with a newer GitHub release |

Several behaviors are shared across commands: the conflict policy (`--on-conflict`), dry-run preview, the verbosity ladder, output-path templates, and run reports. They are explained once in the full reference at [`docs/cli.md`](docs/cli.md), which also lists every flag per command. Run `rom-converto <command> --help` for the same detail in the terminal.

## Desktop GUI

The desktop app runs the same operations as the CLI over the same library, so an equivalent run produces identical output. It runs on Windows, macOS, and Linux, and adds:

- Drag-and-drop batch queues that process many files in one run.
- Live progress and a cancel button that aborts a running conversion and discards its partial output.
- A preview toggle that shows the plan for a run without writing anything.
- A rich info card for inspecting a ROM's metadata and icon.

See [`docs/gui.md`](docs/gui.md) for the page overview and the full CLI parity table.

## Configuration

A TOML config file lets you set per-format default flags and named presets so long flag combinations do not have to be retyped. The config is searched in the current directory and the per-user config directory, and its values sit below command-line flags in precedence. Details and a full example are in [`docs/configuration.md`](docs/configuration.md).

## How it works

The project is a Cargo workspace with four crates:

| Crate | Role |
|---|---|
| `rom-converto-lib` | All conversion, compression, encryption, and verification logic |
| `rom-converto-cli` | Command line interface |
| `rom-converto-gui` | Desktop app (Tauri + Nuxt) |
| `rom-converto-benchmark` | Benchmark harness comparing rom-converto against external reference tools |

Both the CLI and the GUI call the same library functions, so an equivalent run through either front end produces identical output.

## Benchmarks

Compression output is measured against each format's reference encoder (Dolphin, chdman, nsz, and the Azahar Z3DS compressor) on their own defaults. Full methodology and per-run numbers live in the benchmark files:

- [`benchmark/3DS.md`](benchmark/3DS.md): 3DS Z3DS results
- [`benchmark/GameCube.md`](benchmark/GameCube.md): GameCube RVZ results
- [`benchmark/Wii.md`](benchmark/Wii.md): Wii RVZ results
- [`benchmark/Switch.md`](benchmark/Switch.md): Switch NSZ/XCZ results
- [`benchmark/CHD.md`](benchmark/CHD.md): CD/DVD CHD results

Reproduce them with the `rom-converto-benchmark` crate; see [`docs/development.md`](docs/development.md) for the commands.

## Development

You need a recent stable Rust toolchain, plus Node.js 22 or newer and pnpm for the GUI. Run the CLI with `cargo run -p rom-converto-cli` and the GUI with `pnpm tauri dev` from `crates/rom-converto-gui`. The CI gates are `cargo fmt --all -- --check`, `cargo check`, `cargo clippy -- -D warnings`, `cargo test`, and the GUI type-check and build. See [`docs/development.md`](docs/development.md).

## Contributing

Issues and pull requests are welcome. Commits follow [Conventional Commits](https://www.conventionalcommits.org/), because the release version, GitHub Releases, and `CHANGELOG.md` are generated from the commit history.

## License

rom-converto is licensed under the [MIT license](LICENSE).

## Acknowledgments

These projects and resources were helpful during development:

- [Makerom/Ctrtool](https://github.com/3DSGuy/Project_CTR)
- [Cia-Unix](https://github.com/shijimasoft/cia-unix)
- [ctrdecrypt](https://github.com/shijimasoft/ctrdecrypt)
- [make_cdn_cia](https://github.com/llakssz/make_cdn_cia)
- [TikGenerator](https://github.com/matiffeder/TikGenerator)
- [3DSBrew](https://www.3dbrew.org/wiki/Main_Page)
- [decrypt.py](https://gist.github.com/melvincabatuan/3675deef7c58ce13b28236e61917e577)

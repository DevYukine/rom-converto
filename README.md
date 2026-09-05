# rom-converto

[![Test Commit](https://github.com/DevYukine/rom-converto/actions/workflows/tests.yml/badge.svg)](https://github.com/DevYukine/rom-converto/actions/workflows/tests.yml)
[![Latest release](https://img.shields.io/github/v/release/DevYukine/rom-converto)](https://github.com/DevYukine/rom-converto/releases/latest)
[![Sponsor](https://img.shields.io/badge/Sponsor-DevYukine-EA4AAA?logo=githubsponsors&logoColor=white)](https://github.com/sponsors/DevYukine)

rom-converto converts, compresses, verifies, encrypts, and decrypts ROMs and disc images for Nintendo 3DS, GameCube, Wii, Wii U, Nintendo Switch, and CD or DVD media. It ships as a cross-platform command line tool, a desktop GUI, a Rust library, and a C ABI bridge for embedding. Its output matches the established encoder for each format, so a rom-converto file drops straight into the emulators and tools people already use.

## Supported formats

| Platform | Input | Output | Compatible with |
|---|---|---|---|
| Nintendo DS (`nds`) | encrypted or decrypted `.nds` | the opposite state | melonDS, DeSmuME |
| Nintendo 3DS (`ctr`) | `.3ds`, `.cci`, `.cxi`, `.cia`, CDN content | Z3DS | Azahar |
| GameCube (`dol`) | `.iso`, `.gcm`, `.gcz`, NKit | RVZ | Dolphin |
| Wii (`rvl`) | `.iso`, `.wbfs`, `.wia`, `.gcz`, NKit | RVZ | Dolphin |
| Wii U (`wup`) | NUS or loadiine title, `.wud`, `.wux` | WUA | Cemu |
| Switch (`nx`) | NSP, XCI | NSZ, XCZ | nsz |
| CD / DVD (`chd`) | `.cue`+`.bin`, `.iso` | CHD | chdman, PPSSPP, PCSX2 |
| LaserDisc (`chd`) | `.avi` | CHD | MAME |
| PSP / PS2 (`cso`) | `.iso` | CSO, ZSO | maxcso, PPSSPP, Open PS2 Loader |
| CD (`cue`) | `.cue`+`.bin` | merged `.bin`/`.cue` | any emulator |
| Xbox (`xbox`) | full disc image or folder | XISO | xemu |
| Xbox 360 (`xenon`) | full disc image or folder | ZAR, GoD | Xenia |
| PlayStation 3 (`ps3`) | encrypted `.iso` | decrypted `.iso` | RPCS3 |

RVZ and NSZ/XCZ output is byte-identical to the reference encoder (Dolphin, nsz) at matching settings; CSO/ZSO and CHD interoperate with maxcso and chdman. See [`docs/formats.md`](docs/formats.md) for what each format is, where it works, and the compatibility notes.

`rom-converto info <input>` auto-detects the console and inspects any format above, plus NDS ROMs, 3DS homebrew (`.3dsx`), PSP `EBOOT.PBP`, PS Vita `.vpk`, PSN `.pkg` (PSP, PS3, PS Vita), and 14 cartridge systems from NES to Atari 7800. It reports title metadata, embedded icons, encryption state, and a content type normalized to Game, Update, DLC, or Demo across platforms. Single-image commands also read `.zip`, `.7z`, `.rar`, and `.tar` archives directly. Full inspection coverage is in [`docs/cli.md`](docs/cli.md).

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

# Compress a whole folder of Switch games, previewing first
rom-converto nx compress -R ./switch --dry-run
rom-converto nx compress -R ./switch

# Hash a directory and write a report
rom-converto hash -R ./roms --report hashes.csv
```

Add `-R`/`--recursive` to any conversion to process a directory tree, and `--dry-run` to preview a run without writing anything.

## Command line

Each top-level command is a console or format family (`nds`, `ctr`, `dol`, `rvl`, `wup`, `nx`, `chd`, `cso`, `cue`, `xbox`, `xenon`, `ps3`, `psp`, `vita`) with operations such as `compress`, `decompress`, `verify`, and `info`. Standalone commands cover cross-console inspection (`info`), ROM identification against the Playmatch database (`dat`), hashing (`hash`), `.m3u` playlists (`playlist`), plus `capabilities`, `shell-completions`, and `self-update`.

The full reference with every command and flag, and the behaviors shared across commands (conflict policy, dry-run, output-path templates, run reports), is [`docs/cli.md`](docs/cli.md). `rom-converto <command> --help` gives the same detail in the terminal.

## Desktop GUI

The desktop app runs the same operations as the CLI over the same library, so an equivalent run produces identical output. It runs on Windows, macOS, and Linux, and adds drag-and-drop batch queues, live progress with cancel, a dry-run preview toggle, and a rich info card for inspecting a ROM's metadata and icon. See [`docs/gui.md`](docs/gui.md).

## Configuration

A TOML config file lets you set per-format default flags and named presets so long flag combinations do not have to be retyped. Details and a full example are in [`docs/configuration.md`](docs/configuration.md).

## How it works

The project is a Cargo workspace: one library crate holds all conversion logic, and the CLI, desktop GUI, and C ABI bridge are thin front ends over it, so an equivalent run through any of them produces identical output. See [`docs/development.md`](docs/development.md) for the workspace layout and [`docs/ffi.md`](docs/ffi.md) for embedding.

## Benchmarks

Compression output is measured against each format's reference encoder (Dolphin, chdman, nsz, and the Azahar Z3DS compressor) on their own defaults. Per-run numbers: [3DS](benchmark/3DS.md), [GameCube](benchmark/GameCube.md), [Wii](benchmark/Wii.md), [Switch](benchmark/Switch.md), [CHD](benchmark/CHD.md). Methodology and reproduction commands are in [`docs/development.md`](docs/development.md).

## Development

You need a recent stable Rust toolchain, plus Node.js 22 or newer and pnpm for the GUI. Setup, dev commands, and the CI gates are in [`docs/development.md`](docs/development.md).

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

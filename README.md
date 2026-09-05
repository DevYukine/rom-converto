# rom-converto

[![Tests](https://github.com/DevYukine/rom-converto/actions/workflows/tests.yml/badge.svg)](https://github.com/DevYukine/rom-converto/actions/workflows/tests.yml)
[![Latest release](https://img.shields.io/github/v/release/DevYukine/rom-converto)](https://github.com/DevYukine/rom-converto/releases/latest)
[![Sponsor](https://img.shields.io/badge/Sponsor-DevYukine-EA4AAA?logo=githubsponsors&logoColor=white)](https://github.com/sponsors/DevYukine)

Convert, compress, decrypt, and verify ROMs and disc images. Available as a command line tool and desktop app for Windows, macOS, and Linux.

[Download](https://github.com/DevYukine/rom-converto/releases/latest) · [Quick start](#quick-start) · [CLI reference](docs/cli.md) · [Desktop app](docs/gui.md)

## Supported consoles and formats

| Console or media | Main operations |
|---|---|
| Nintendo DS | Encrypt and decrypt NDS ROMs |
| Nintendo 3DS | Encrypt, decrypt, and convert CIA/CCI; compress decrypted ROMs to Z3DS; build CIA from CDN content |
| GameCube | Compress ISO/GCM to RVZ; migrate GCZ and NKit to RVZ; decompress RVZ |
| Wii | Compress ISO/WBFS to RVZ; migrate GCZ, WIA, and NKit to RVZ; decompress RVZ |
| Wii U | Pack NUS, loadiine, WUD, or WUX into WUA; decrypt NUS to a game folder |
| Nintendo Switch | Compress NSP/XCI to NSZ/XCZ and decompress them; merge or split unpacked NSP/XCI |
| CD / DVD | Compress CUE/BIN or ISO to CHD; migrate CHD v1 to v4 into v5; extract CHD; merge CUE/BIN tracks |
| LaserDisc | Compress AVI to CHD |
| PSP / PS2 | Compress ISO to CSO/ZSO; decompress CSO/ZSO/DAX; convert between these and CHD |
| PSP | Extract EBOOT.PBP and PKG; convert supported packages to ISO |
| PS Vita | Extract PKG files |
| Xbox | Convert a disc image or game folder to XISO; extract disc files |
| Xbox 360 | Pack a disc image or game folder into ZAR; convert a disc ISO to GoD; extract ZAR files |
| PlayStation 3 | Decrypt disc ISOs |

`info` also inspects PSP, PS Vita, PSN packages, and classic cartridge ROMs. Inspection support does not imply conversion support. See [formats and compatibility](docs/formats.md) for extensions, limits, and emulator guidance.

**Preview first:** add `--dry-run` to a conversion to see planned outputs. Use `-R` (`--recursive`) on commands that support folder scans.

## Install

Download the CLI or desktop app for your system from [Releases](https://github.com/DevYukine/rom-converto/releases/latest). CLI releases are standalone binaries. Rename yours to `rom-converto` (`rom-converto.exe` on Windows) and add its folder to your `PATH`. On Linux and macOS, run `chmod +x rom-converto` first.

```sh
rom-converto --version
rom-converto --help
```

Switch conversions need `prod.keys`. Wii U and PS3 disc operations resolve disc keys automatically where possible. See [key setup](docs/configuration.md).

## Quick start

Compress one GameCube image, or preview a whole folder:

```sh
rom-converto dol compress game.iso
rom-converto dol compress -R ./games --output-dir ./rvz --dry-run
```

Run the batch after reviewing the preview:

```sh
rom-converto dol compress -R ./games --output-dir ./rvz
```

Other common tasks:

```sh
# Restore an RVZ to ISO
rom-converto dol decompress game.rvz

# Decrypt a 3DS ROM
rom-converto ctr decrypt game.cia

# Compress a Switch game
rom-converto nx compress game.nsp --keys prod.keys

# Inspect a file
rom-converto info game.iso

# Hash a folder and save the results
rom-converto hash -R ./games --report hashes.csv
```

Most conversions derive the output name from the input. Use an explicit output path or `--output-dir` where supported.

### Useful options

| Option | Use it to |
|---|---|
| `--dry-run` | Preview a conversion, rename, or playlist operation before running it |
| `-R`, `--recursive` | Process matching files in a folder and its subfolders |
| `--max-depth 1` | Limit a supported folder scan to the top level |
| `--output-dir ./converted` | Choose an output folder |
| `--on-conflict skip` | Keep existing outputs and skip those files |
| `-f`, `--force` | Overwrite existing outputs |
| `--report run.json` | Save results as JSON, CSV, or HTML, based on the extension |
| `--config settings.toml`, `--preset fast` | Reuse defaults and named settings |
| `-h`, `--help` | Show options for the selected command |

Options vary by command. Check `rom-converto dol compress --help`, for example. Dry runs can still read inputs, contact services, and write requested logs or reports; see [CLI behavior](docs/cli.md).

## Documentation

| Guide | Contents |
|---|---|
| [CLI reference](docs/cli.md) | Every command and option, output paths, batch behavior, reports |
| [Formats](docs/formats.md) | Supported inputs, outputs, verification, compatibility |
| [Configuration](docs/configuration.md) | Config files, presets, key lookup |
| [Desktop app](docs/gui.md) | Queues, inspection, settings, updates |
| [Development](docs/development.md) | Build, test, workspace layout, benchmarks |
| [C API](docs/ffi.md) | Embed the library in another application |

## Development

Build the CLI with a current stable Rust toolchain:

```sh
cargo build --release -p rom-converto-cli
```

The binary is `target/release/rom-converto` (`rom-converto.exe` on Windows). Desktop development also uses Node.js 22 and pnpm. See [development setup](docs/development.md) for platform dependencies and checks.

### Built with

- **Rust** for conversion logic, a **Clap** CLI, and **Tokio** for asynchronous work.
- **Tauri 2**, **Nuxt 4 / Vue**, **TypeScript**, **Tailwind CSS**, and **Pinia** for the desktop app.
- **RustCrypto** crates for encryption and hashing; **Zstandard**, **LZMA**, **zlib**, and other codecs for format-specific compression.
- A shared Rust library used by the CLI, desktop app, and C API.

### Benchmarks

See results for [3DS](benchmark/3DS.md), [GameCube](benchmark/GameCube.md), [Wii](benchmark/Wii.md), [Switch](benchmark/Switch.md), and [CHD](benchmark/CHD.md). [Development](docs/development.md) covers how to reproduce them.

## Contributing

Issues and pull requests are welcome. Keep changes focused and run the checks in the [development guide](docs/development.md). Use [Conventional Commits](https://www.conventionalcommits.org/); releases and the changelog are generated from commit history.

## License

[MIT](LICENSE).

## Acknowledgments

The original 3DS work builds on these projects and resources:

- [Makerom/Ctrtool](https://github.com/3DSGuy/Project_CTR)
- [Cia-Unix](https://github.com/shijimasoft/cia-unix)
- [ctrdecrypt](https://github.com/shijimasoft/ctrdecrypt)
- [make_cdn_cia](https://github.com/llakssz/make_cdn_cia)
- [TikGenerator](https://github.com/matiffeder/TikGenerator)
- [3DSBrew](https://www.3dbrew.org/wiki/Main_Page)
- [decrypt.py](https://gist.github.com/melvincabatuan/3675deef7c58ce13b28236e61917e577)

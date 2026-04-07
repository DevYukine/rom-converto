# rom-converto

A command-line utility suite for converting, compressing, encrypting, and decrypting ROM formats, currently focused on the **Nintendo 3DS** and **CD image** formats.

Built for developers, tinkerers and archivists.

## Features

### Currently Supported

* [x] Convert 3DS CDN files to `.cia` format
* [x] Generate 3DS Tickets for CDN files
* [x] Decrypt `.cia` files for usage on emulators (e.g. [Azahar](https://azahar-emu.org/))
* [x] Compress and decompress 3DS ROMs using the Z3DS format (seekable zstd)
* [x] Compress CD images (`.bin` + `.cue`) to `.chd` format
* [x] Extract `.chd` files back to `.bin` + `.cue`
* [x] Verify `.chd` file integrity (SHA1 checksums)
* [x] Self-update from GitHub releases

## Commands

### CTR (Nintendo 3DS)

| Command | Description |
|---|---|
| `ctr cdn-to-cia <CDN_DIR> [OUTPUT]` | Convert a CDN directory to `.cia` |
| `ctr generate-cdn-ticket <CDN_DIR> [OUTPUT]` | Generate a `.tik` ticket from CDN content |
| `ctr decrypt-cia <INPUT> <OUTPUT>` | Decrypt an encrypted `.cia` for emulator use |
| `ctr compress <INPUT> [OUTPUT]` | Compress a decrypted ROM to Z3DS format |
| `ctr decompress <INPUT> [OUTPUT]` | Decompress a Z3DS file back to the original ROM |

**`cdn-to-cia` flags:**

| Flag | Description |
|---|---|
| `-C, --cleanup` | Remove original CDN files after conversion |
| `-R, --recursive` | Process all subdirectories, converting each to `.cia` |
| `-T, --ensure-ticket-exists` | Auto-generate a ticket file if one is not found |
| `-D, --decrypt` | Also decrypt the CIA after creation |

> **`generate-cdn-ticket`:** Generated tickets use placeholder values (null Console ID, etc.) and only work on modded consoles and emulators. They will not work on stock hardware.

> **`decrypt-cia`:** Place a `seeddb.bin` file next to the executable to resolve seeds locally. If none is found, the tool will fetch the required seed from Nintendo's API.

> **`compress` / `decompress`:** Supported input formats for compression: `.cia`, `.cci`, `.3ds`, `.cxi`, `.3dsx`. Output files use the Z3DS format (`.zcia`, `.zcci`, `.zcxi`, `.z3dsx`). Compression requires a decrypted ROM — encrypted ROMs have near-zero compression ratios. The output file path defaults to the input path with the extension updated automatically.

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

## Development

### Prerequisites

1. Install Rust 1.88+ from [here](https://www.rust-lang.org/tools/install)

### Running in Development

1. Clone the repository
2. Run `cargo run --package rom-converto --bin rom-converto` to run the CLI

### Building a Release Binary

```
cargo build --release
```

The resulting binary will be at `target/release/rom-converto`.

## Built With

* [Rust](https://www.rust-lang.org/) - The programming language used
* [tokio](https://tokio.rs/) - Asynchronous runtime
* [clap](https://github.com/clap-rs/clap) - Command-line argument parsing
* [binrw](https://github.com/jam1garner/binrw) - Reading/writing binary data structures
* [RustCrypto](https://github.com/rustcrypto) - AES encryption, key derivation, and hashing
* [flate2](https://github.com/rust-lang/flate2-rs) / [lzma-sdk-sys](https://github.com/nicowillis/lzma-sdk-sys) / [flacenc](https://github.com/yotarok/flacenc-rs) - CHD compression codecs
* [indicatif](https://github.com/console-rs/indicatif) - Progress bars

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

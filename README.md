# rom-converto

A command-line utility suite for converting, compressing, encrypting, and decrypting ROM formats, currently focused on the **Nintendo 3DS**.

Built for developers, tinkerers and archivists.

## Features

### Currently Supported

* [x] Convert 3DS CDN files to `.cia` format
* [x] Generate 3DS Tickets for CDN files
* [x] Decrypts `.cia` files for usage on emulators (e.g. [Azahar](https://azahar-emu.org/))


## Development

### Prerequisites

1. Install Rust 1.88+ from [here](https://www.rust-lang.org/tools/install)

### Running in Development

1. Clone the repository
2. Run `cargo run --package rom-converto --bin rom-converto` to run the cli

The resulting binary will be in `target/release/rom-converto`.

## Built With

* [Rust](https://www.rust-lang.org/) - The programming language used
* [tokio](https://tokio.rs/) - Asynchronous runtime
* [clap](https://github.com/clap-rs/clap) - Command-line argument parsing
* [binrw](https://github.com/jam1garner/binrw) - Reading/writing binary data structures
* [RustCrypto](https://github.com/rustcrypto) - Libraries for cryptographic operations

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

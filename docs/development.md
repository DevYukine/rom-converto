# Development

## Requirements

- Current stable Rust, installed with [rustup](https://rustup.rs/).
- Node.js 22 and pnpm 11 for the desktop app.

For the desktop app, follow the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform, including Windows build tools or macOS Xcode tools. Ubuntu CI installs:

```sh
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev xdg-utils
```

This is a Rust 2024 Cargo workspace:

| Crate | Purpose |
| --- | --- |
| `rom-converto-lib` | Conversion, verification, configuration, and JSON runner code. |
| `rom-converto-cli` | Command-line application. |
| `rom-converto-gui` | Tauri desktop application. Its Nuxt frontend is in `crates/rom-converto-gui`. |
| `rom-converto-ffi` | C ABI embedding library. |
| `rom-converto-benchmark` | Reference-tool comparison harness. |

All front ends call `rom-converto-lib`.

## Run and build

Run the CLI from the workspace root:

```sh
cargo run -p rom-converto-cli -- dol compress game.iso
```

Run the desktop app:

```sh
cd crates/rom-converto-gui
pnpm install --frozen-lockfile
pnpm tauri dev
```

Build release artifacts:

```sh
cargo build --release -p rom-converto-cli
cargo build --release -p rom-converto-ffi
cd crates/rom-converto-gui
pnpm tauri build
```

The CLI is `target/release/rom-converto`. The FFI build produces a `cdylib` in
`target/release`; see [FFI](ffi.md). Development builds display `dev-<git short
hash>`. Set `ROM_CONVERTO_RELEASE=1` to display the semantic version, as release
automation does.

## Checks

The GitHub workflow runs these Rust checks:

```sh
cargo fmt --all -- --check
cargo check -p rom-converto-lib -p rom-converto-cli -p rom-converto-benchmark -p rom-converto-ffi
cargo test -p rom-converto-lib -p rom-converto-cli -p rom-converto-benchmark -p rom-converto-ffi
cargo clippy -p rom-converto-lib -p rom-converto-cli -p rom-converto-benchmark -p rom-converto-ffi -p rom-converto-gui -- -W clippy::unwrap-used -D warnings
```

Frontend unit tests use Vitest:

```sh
cd crates/rom-converto-gui
pnpm install --frozen-lockfile
pnpm test
```

`pnpm build` generates the Nuxt static frontend. CI packages the GUI with Tauri
on Windows, macOS, and Linux.

## Reference-tool tests and benchmarks

Optional parity tests need the reference program:

```sh
ROMCONVERTO_CHDMAN=$(which chdman) cargo test -p rom-converto-lib chdman
ROMCONVERTO_MAXCSO=$(which maxcso) cargo test -p rom-converto-lib maxcso
```

Run a benchmark after a release CLI build:

```sh
cargo build --release -p rom-converto-cli
cargo run -p rom-converto-benchmark -- <platform> [inputs]
```

Supported platforms are `switch`, `wii`, `gamecube`, `chd`, and `ctr` (`3ds` is
an alias). Reference tools must be on `PATH` or next to the CLI. Inputs can come
from command options or `ROMCONVERTO_BENCH_*` variables; `all` runs each platform
whose variables are set.

## Embedded DS key table

`resources/nds_blowfish.bin` is the 4,168-byte Nintendo DS KEY1 table embedded in
the binary. Regenerate it from the `encr_data` literals in devkitPro
[ndstool](https://github.com/devkitPro/ndstool) `source/encryption.cpp` at blob
`0de8f088b79a0e73bc31601f767ee674fa31badb`, preserving byte order. Its SHA-256 is
`bedd20bd7f9cac742ad760e2448d4043e0d37121b67a1be3a6b8afbb8a34f08e`.

## Documentation and releases

Public `rom-converto-lib` APIs need Rust documentation. Start modules with a
short `//!` description, use a one-line third-person summary for public items,
and add `# Errors`, `# Panics`, or `# Safety` when applicable. Prefer intra-doc
links.

Use Conventional Commit messages. Release automation derives the version,
GitHub release, and `CHANGELOG.md` from commit history, so do not edit the
changelog by hand.

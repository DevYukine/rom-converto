# Development

## Prerequisites

- A recent stable Rust toolchain. Install it from [rustup](https://www.rust-lang.org/tools/install).
- For the GUI: [Node.js 22+](https://nodejs.org/) and [pnpm](https://pnpm.io/installation).

The project is a Cargo workspace with five crates: `rom-converto-lib` (all conversion,
compression, encryption, verification, and shared JSON runner logic), `rom-converto-cli`
(the command line interface), `rom-converto-gui` (the Tauri desktop app),
`rom-converto-benchmark` (a harness that compares rom-converto against reference tools), and
`rom-converto-ffi` (the C ABI bridge). All front ends call the same library code.

## Running in development

Run the CLI directly from the workspace:

```
cargo run -p rom-converto-cli -- dol compress game.iso
```

Run the GUI from its crate directory. The GUI uses pnpm, not npm:

```
cd crates/rom-converto-gui
pnpm install
pnpm tauri dev
```

Local development builds show the version as `dev-<shorthash>` from the current git commit;
the same applies to the GUI version label. If the source is not a git checkout, the build
falls back to the plain semantic version.

## Building release binaries

```
cargo build --release -p rom-converto-cli
```

The binary lands at `target/release/rom-converto`. Set `ROM_CONVERTO_RELEASE=1` at build time
to mark a release build, which shows the semantic version instead of the dev hash. The release
CI workflow sets this automatically.

Build the C ABI bridge with:

```
cargo build --release -p rom-converto-ffi
```

That produces the platform `cdylib` under `target/release` (`rom_converto_ffi.dll`,
`librom_converto_ffi.so`, or `librom_converto_ffi.dylib`). Windows also produces
`rom_converto_ffi.dll.lib`, the link-time import library. Unix release assets are
`rom-converto-ffi-<classifier>.tar.gz`; Windows assets are
`rom-converto-ffi-<classifier>.zip`. Each extracts to a
`rom-converto-ffi-<classifier>` directory with the library,
`include/rom_converto.h`, and `LICENSE`. The header is the C ABI source of truth.
See [`ffi.md`](ffi.md) for its ownership, threading, callback, cancellation, and JSON
contracts.

For the GUI, build the Tauri bundle:

```
cd crates/rom-converto-gui
pnpm install
pnpm tauri build
```

## Cross-tool parity tests

Some tests cross-check output against the reference tools when they are available. They are
skipped unless an environment variable points at the binary:

```
ROMCONVERTO_CHDMAN=$(which chdman) cargo test -p rom-converto-lib chdman
ROMCONVERTO_MAXCSO=$(which maxcso) cargo test -p rom-converto-lib maxcso
```

## Running benchmarks

The `rom-converto-benchmark` crate runs the compression comparisons behind the
`benchmark/*.md` numbers with the same methodology on your own hardware. Build the
release binary first, then run a platform:

```
cargo build --release -p rom-converto-cli
cargo run -p rom-converto-benchmark -- <platform> [inputs]
```

| Platform subcommand | Reference tool | Input flags |
|---|---|---|
| `switch` | `nsz` | `--nsp`, `--xci`, `--keys` |
| `wii` | `DolphinTool` | `--iso`, `--levels` |
| `gamecube` | `DolphinTool` | `--iso`, `--levels` |
| `chd` | `chdman` | `--cue` (with a sibling `.bin`) |
| `ctr` (alias `3ds`) | `z3ds_compressor` | `--three-ds`, `--cia` (both decrypted) |

Each reference tool must be installed and either on your `PATH` or placed next to the
rom-converto binary. A missing tool stops the run with a message naming the tool to
install. Inputs can also come from the `ROMCONVERTO_BENCH_*` environment variables, and
`rom-converto-benchmark all` runs every platform whose variables are set.

## CI gates

Every change runs these checks. Run them locally before opening a pull request:

```
cargo fmt --all -- --check
cargo check -p rom-converto-lib -p rom-converto-cli -p rom-converto-benchmark -p rom-converto-ffi
cargo clippy -p rom-converto-lib -p rom-converto-cli -p rom-converto-benchmark -p rom-converto-ffi -- -D warnings
cargo test -p rom-converto-lib -p rom-converto-cli -p rom-converto-benchmark -p rom-converto-ffi
```

For the GUI, from `crates/rom-converto-gui`:

```
pnpm exec nuxt prepare
pnpm exec vue-tsc --noEmit
pnpm run build
```

## Releases

Commits follow [Conventional Commits](https://www.conventionalcommits.org/). The release
version, GitHub Releases, and `CHANGELOG.md` are generated from the commit history, so
`CHANGELOG.md` is never hand-edited. Write commit messages that describe the change in the
Conventional Commits format (`feat:`, `fix:`, `docs:`, `refactor:`, and so on) and the release
automation does the rest.

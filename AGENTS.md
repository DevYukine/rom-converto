# Contributor guide for rom-converto

This file is for everyone who changes this repository, including coding agents
that read `AGENTS.md`. It states how the project is built, reviewed, and
released. Reviews apply these rules, so read it before the first edit.

## Repository map

Rust 2024 Cargo workspace. All front ends call `rom-converto-lib`; nothing
else contains conversion logic.

| Path | Role |
| --- | --- |
| `crates/rom-converto-lib` | Formats, verification, config, and the JSON runner (`runner::run_request`, `runner::run_json_with_progress`) that the CLI, GUI, and FFI call. |
| `crates/rom-converto-cli` | Clap CLI. Binary `rom-converto`. One file per command family in `src/commands/`. |
| `crates/rom-converto-gui` | Nuxt 4 + Vue + Pinia frontend. `src-tauri/` is the Tauri 2 shell and the Cargo member. |
| `crates/rom-converto-ffi` | C ABI (`cdylib`). Header `include/rom_converto.h` is hand-maintained. |
| `crates/rom-converto-benchmark` | Reference-tool comparison harness. CI compiles and lints it but never runs a benchmark. |
| `docs/` | User docs: `cli.md`, `gui.md`, `formats.md`, `configuration.md`, `ffi.md`, `development.md`. |
| `resources/` | Embedded key tables and cert chains. `docs/development.md` explains how `nds_blowfish.bin` is regenerated. |
| `benchmark/` | Published benchmark results. |

Inside the library:

- One module per platform or format, for example `nintendo/{ctr,dol,rvl,wup,nx,ntr,disc}`, `sony/`, `microsoft/`, `sega/`, `disc/{chd,cue,...}`, `cso/`, `zar/`, `dat/`, `patch/`, plus `info/`, `config/`, `playlist/`.
- A format module has `error.rs` (a `thiserror` enum), `info.rs`, `verify.rs`, and `test_fixtures.rs` where applicable. Follow that layout for new formats.
- `runner/` is the single operation registry: `ops.rs` (`OpSpec` rows in `OPS`, handlers), `ops_misc.rs`, `ops_ms.rs`, `ops_sony.rs`, `models.rs` (`RunRequest`, `RunOptions`, `RunResponse`), `cli_echo.rs` (the tables that map runner options to CLI flags), `organize/`.
- `util/` holds shared pieces: `ProgressReporter`, `CancelToken`, archive staging, conflict handling, atomic writes, reports, the hash cache, worker pools.

## Workflow

- `develop` is the working branch. `main` only receives release commits. Base every branch on `develop`.
- Branch names: `feat/<slug>`, `fix/<slug>`, `perf/<slug>`, `chore/<slug>`, `docs/<slug>`.
- Pull requests target `develop` and are squash-merged. The PR title becomes the commit subject and, for `feat`, `fix`, and `perf`, the changelog line. Write it as a Conventional Commit.
- PR description: a short summary of what and why. No checklists, no test plan sections, no tool or assistant attribution, no `Co-Authored-By` or "Generated with" lines.
- Keep "Allow edits by maintainers" enabled. The maintainer may push fixes onto your branch before merging.
- Keep each PR to one change. Do not bundle refactors, reformatting, or unrelated fixes with a feature.
- Never pass `--no-verify` or `--no-gpg-sign`.

### Commit messages

Conventional Commits, one line, no body:

```
fix(chd): extract CD images to one bin per track
feat(nx): decrypt nsp and xci into nxemu dnsp and dxci across lib, cli and gui
perf(wup): decrypt by range and stream titles into wua
docs(cli): align organize help with sibling commands
```

- Use these types: `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `chore`, `ci`.
- Scope is the console or feature area: `ctr`, `dol`, `rvl`, `wup`, `nx`, `nds`, `chd`, `cso`, `cue`, `psp`, `ps3`, `vita`, `xbox`, `xenon`, `dat`, `hash`, `organize`, `patch`, `archive`, `info`. Crate scopes are `lib`, `cli`, `gui`, `ffi`, `runner`, `release`. Join several with commas: `fix(dol,rvl): ...`. Cross-cutting commits may omit it.
- Start with a lowercase verb. Describe the fix or feature itself. Never name another tool as the reference in the subject ("like Dolphin", "match nsz", "per Atmosphere"). The changelog is generated from these lines and must read cleanly on its own.
- Never edit `CHANGELOG.md` or the workspace version in `Cargo.toml`. Release automation on `main` bumps both and commits `chore(release): vX.Y.Z [skip ci]`.

## Checks before you push

CI uses the floating stable toolchain, so update yours first and run exactly what CI runs (`.github/workflows/tests.yml`):

```sh
rustup update stable
cargo fmt --all -- --check
cargo check -p rom-converto-lib -p rom-converto-cli -p rom-converto-benchmark -p rom-converto-ffi
cargo test -p rom-converto-lib -p rom-converto-cli -p rom-converto-benchmark -p rom-converto-ffi -p rom-converto-gui
cargo clippy -p rom-converto-lib -p rom-converto-cli -p rom-converto-benchmark -p rom-converto-ffi -p rom-converto-gui -- -W clippy::unwrap-used -D warnings
```

Whenever a type exported to TypeScript or a `cli_echo` table changes, regenerate the bindings and commit the result. CI deletes `types/generated` and fails on any diff:

```sh
cargo test -p rom-converto-gui --features ts-export ts_export
```

Frontend changes:

```sh
cd crates/rom-converto-gui
pnpm install --frozen-lockfile
pnpm test
pnpm typecheck
```

Test, clippy, and fmt jobs run on Ubuntu only. `cfg(windows)` and `cfg(target_os = "macos")` code is compiled only by the build workflows on the PR, so a mistake there surfaces late. Add matching `#[cfg]` attributes to platform-only helpers and keep those builds warning-free. If you cannot compile for that target locally, say so in the PR.

Optional parity tests run only when their variable points at the reference binary (`ROMCONVERTO_CHDMAN`, `ROMCONVERTO_MAXCSO`, `ROM_CONVERTO_DOLPHIN_TOOL` with its `ROM_CONVERTO_DOLPHIN_PARITY_*` inputs) or enables a live call (`ROM_CONVERTO_PLAYMATCH_LIVE`). Gate new parity tests the same way: return early when the variable is unset.

## Code rules

These apply to every language in the repository.

- Write the minimum code that solves the problem. No speculative abstractions, no features nobody asked for, no configuration for hypothetical needs.
- Surgical diffs. Touch only what the task requires. No drive-by reformatting, renames, or "improvements" outside the task.
- Read the neighbouring code first and match its patterns. Rust looks like Rust, TypeScript like TypeScript.
- Comments explain non-obvious why. They never restate the code. No banner comments, no numbered-step comments, no commented-out code.
- No defensive layers around things that cannot fail. Let errors propagate unless there is a real recovery path.
- No dead code, no unused re-exports, no `TODO: implement` placeholders. A PR is complete or it is a draft.
- Performance is deliberate in hot paths (decoders, hashing, large copies). Choose data structures and streaming on purpose and measure before and after. Do not micro-optimise cold paths at the cost of readability.
- Indentation: Rust is whatever `cargo fmt` produces. In the frontend, match the indentation of the file you edit. YAML uses two spaces. Line endings are LF everywhere (`.gitattributes`).
- If a requirement is ambiguous, state your interpretation in the PR or ask. Do not pick silently.
- Treat external services and other repositories as read-only. Do not change CI, release automation, or upstream projects as part of a feature; ask first.

### Rust

- Never `unwrap` outside tests; CI denies it. Use `expect` only for a real invariant, with a message that says why it holds. `clippy.toml` allows both in tests.
- Errors: a module with an `error.rs` uses its `thiserror` enum; follow the error style of the module you edit. Give every new format module an `error.rs`. Marker errors such as `util::Cancelled` and `util::conflict::OutputExists` drive batch semantics; propagate them unchanged.
- Make every operation handler `async`, take a `&dyn ProgressReporter` and a `CancelToken`, check cancellation inside long loops, and run blocking codec work through `spawn_blocking_with_progress` or the worker pool. Never block the Tokio runtime.
- Keep memory bounded. Stream large data, read by range, and validate header-declared sizes and table counts before allocating. A corrupt input must fail with an error, not with an allocation of its claimed size.
- Treat every input as untrusted. Member names from archives, title file names, and cue sheet paths must never write outside the chosen output folder. Reject absolute, drive-relative, and escaping names.
- Write outputs through `util::atomic_write` or `scratch_output_path`: a sibling `.tmp` file in the output directory, published by rename once the write succeeded. Remove partial output on cancel or failure. Delete a source only after its output was written successfully.
- Keep new `unsafe` to C codec bindings, `libc` calls, and the FFI crate. Give each new block a `// SAFETY:` comment and each unsafe function a `# Safety` section.
- Document public `rom-converto-lib` items: a `//!` module summary, a one-line third-person summary per item, `# Errors`, `# Panics`, or `# Safety` where applicable, intra-doc links.
- Do not add `target_env` cfgs. The same code must build for gnu and msvc targets.
- FFI: change the exported functions, `include/rom_converto.h`, and `docs/ffi.md` together. Every boundary keeps its `catch_unwind`. A breaking change to the JSON run schema or the C ABI is a maintainer decision; ask before bumping `RUN_SCHEMA` or `ABI_VERSION`.

### Naming other tools in code

Code and doc comments describe this implementation. Do not name a third-party tool as the authority ("matches chdman", "ported from nsz", "byte-identical to Dolphin"). Say "the reference implementation", "the upstream implementation", "the spec", or "other tools", and keep upstream file or function names in backticks (for example `cdrom.cpp`).

Names that may stay:

- Emulators, firmware, and loaders as consumers of our output: Dolphin, Cemu, PPSSPP, Xenia, Atmosphere, yuzu, NxEmu, Azahar.
- Format names: NSZ, NKit, CHD, ZArchive, RVZ.
- Identifiers and environment variables, such as `ROMCONVERTO_CHDMAN`.
- Integrations: Playmatch, RomM, frontend layout tokens.
- DAT groups (No-Intro, Redump, TOSEC), hardware wikis, and our own dependencies.

This rule does not apply to `docs/*.md`, the README, clap help text, UI strings, or the benchmark crate.

### CLI

- Clap derive. The `///` doc comment on every argument is the help text, so write it as user documentation: plain words, imperative, no internals.
- Reuse the shared groups in `commands/mod.rs`: `OutputArgs`, `ConflictArgs` (`--on-conflict`, `-f/--force`), `BatchArgs` (`--max-depth`, `--report`). Declare `-R/--recursive` per command with help text that names the extensions it scans. Precedence is flag, then preset, then config file, then built-in default.
- `--dry-run` plans through the same code as a real run and writes no conversion output. Reports and logs that the user asked for are still written.
- `--report` chooses CSV, JSON, or HTML from the file extension.
- Document every new flag or subcommand in `docs/cli.md` in the same PR. Keep sibling commands consistent: same flag names, same wording, same grouping.

## Adding or changing an operation

Operations flow lib -> runner -> front ends. A new operation touches all of these:

1. Library: the implementation module, its `error.rs`, and `info`/`verify` support where the format allows it. Unit tests with synthetic fixtures.
2. Runner: an `OpSpec` row in `runner/ops.rs` `OPS` and a handler in `ops.rs` or one of the `ops_*.rs` files. New options are fields on `RunOptions` in `runner/models.rs`.
3. CLI echo: rows in `runner/cli_echo.rs` (`FIELDS`, `PATH_FLAGS`, `PATH_OUTPUT`). The test `cli_echo_derivations_parse` in `rom-converto-cli/src/commands/mod.rs` checks that every derived command parses; add required arguments to `REQUIRED_EXTRA` there.
4. CLI: a subcommand in the family's file under `rom-converto-cli/src/commands/`, new extensions in `ALL_IMAGE_EXTS` (`commands/support.rs`).
5. GUI: an op definition in `lib/opdefs/*.ts`, a store via `makeOpStore` in `stores/<op>.ts`, regenerated `types/generated`. Add mock data in `lib/ipc-mock.ts` only for a new Tauri command or a new result shape.
6. Docs as listed under Documentation, plus the README table if a console or format is new.

The FFI needs no change unless the JSON contract changes.

## Tests

- Rust unit tests live in `#[cfg(test)] mod tests` at the bottom of the file they test. Only the CLI has a `tests/` directory. Frontend tests are colocated `*.test.ts` files.
- Fixtures are synthetic and built in memory or in a `tempfile` directory. Never commit ROMs, keys, or any copyrighted data, and never require them for a test to pass. Never `git add` a key file such as `prod.keys`.
- Test behaviour, boundaries, invariants, state transitions, and error paths. Do not pin wording, incidental defaults, or implementation details. Never delete or skip a test to make it pass.
- Before claiming a format works, verify with real data on your machine. Use round trips that must reproduce the input byte for byte (archive rewrites, compress then decompress), outputs compared against the reference tool, or files that load in the target emulator. State what you verified in the PR. Delete the test files afterwards; do not add them to the repository.

## GUI and UX requirements

The desktop app is the CLI with a window. Users must be able to learn one and predict the other.

### Parity with the CLI

- Map every GUI option to a CLI flag with the same name and meaning. Group options as `docs/cli.md` groups them. Document any default that differs from the CLI in `docs/gui.md`.
- Each page shows the equivalent CLI command. `buildCliCommand` in `composables/useCliEcho.ts` builds it from the generated `cli_echo` manifest; never assemble command strings by hand.
- Dry run uses the same planner as `--dry-run` and writes no conversion output.
- Inspect reads files through the same path as `rom-converto info`.
- Document a GUI behaviour that differs from the CLI (for example conflict handling on a page, folder scanning that creates one job per file) in `docs/gui.md` in the same PR. Deliberately CLI-only features are listed there too; do not add pages for them without discussion.

### Safety

- Write pages offer `On conflict` with Overwrite, Skip, Rename, Error, and Overwrite if invalid. Skip and Error leave existing targets untouched. Organize always starts at Error regardless of the default setting.
- Delete a source (Organize's Move) only after its organized output was written successfully. Skipped or failed items keep their sources.
- Cancel stops current work and removes partial output. Completed items stay completed.
- Check free space before a write (input size plus 256 MiB) unless the user turns on Skip free-space check.
- Disable installing an update while jobs run.

### Visual rules

- `:root` tokens are the dark theme; light overrides live under `[data-theme="light"]`. Define both for every new surface. Take colours, text levels, and alpha fills from the variables in `assets/css/tokens.css`. No hard-coded colours in components.
- Style with scoped component CSS. Tailwind is loaded for its base reset only; do not use utility classes.
- Use the shared tokens for sizes, radii, and font sizes where `tokens.css` defines one; do not add ad hoc values next to an existing token. Keep text at 4.5:1 contrast or better against its background in both themes.
- Give every interactive element a visible `:focus-visible` outline and make it keyboard operable. `prefers-reduced-motion` is respected globally; do not add animation that bypasses it.
- The layout works from the window minimum in `src-tauri/tauri.conf.json` up to very wide windows (pages cap their width) and at every Interface scale offered in Settings. Check a narrow window before submitting.
- Truncate long paths so the file name, extension, and disc number stay visible. Render new long lists (results, plans) with `VirtualList`.
- Icons are inline SVG paths. There is no icon library and no i18n layer. Write UI strings inline in plain English, sentence case, no emoji.

### Interaction rules

- Label on the left, control on the right; the row stacks when the container is narrow.
- Pickers are buttons that show the chosen value. Show the default for an unset value as a placeholder, not as a literal sentence stored in the field.
- Open a collapsed option section when one of its fields gains a value, not on every edit.
- Show caveats and streamed warnings as an inline notice on the page, not as modal dialogs.
- Use `ModalShell` for modals and `ConflictPopover` for the conflict choice.
- Match the terms in `docs/gui.md` and the CLI: "On conflict", "Output directory", "Recursive". Introduce a new term only with a docs update.

### Frontend structure

- Components are PascalCase `.vue` files: `components/shell` (app chrome), `components/op` (`OpPage` and per-feature views), `components/ui` (primitives such as `PrimaryButton`, `Segmented`, `ToggleSwitch`, `StatusTag`, `KvRow`, `InfoTooltip`, `FieldLabel`, `ConfigCard`, `VirtualList`), `components/modals`. Reuse a primitive before writing a new control.
- One Pinia store per operation in `stores/<op>.ts`, created with `makeOpStore`. Cross-cutting state lives in `queue.ts`, `ui.ts` (persisted preferences), `alerts.ts`, `config.ts`.
- `lib/ipc.ts` is the only seam for invoke, events, and dialogs. In a plain browser under `pnpm dev` it loads `lib/ipc-mock.ts`, so every page can be exercised without a native build. `globalThis.__mockOpenPath` stages dialog results; `__mockFailNext` or a path containing `fail` produces a failed job.
- Tauri commands are `#[tauri::command]` functions in `src-tauri/src`, registered in `generate_handler!`. Prefix new ones with `cmd_`. Add a capability permission only when a command needs it.
- Never edit files under `types/generated` by hand.

### Verifying UI work

Run `pnpm dev` and open the page in a browser, then `pnpm tauri dev` for the native shell. Check both themes, a narrow window, keyboard navigation, and a failed job. Attach before and after screenshots to the PR for visual changes.

## Documentation

- Update the docs in the same PR as a user-facing change: `docs/cli.md` for flags, `docs/gui.md` for pages, `docs/formats.md` for inputs, outputs, limits, and emulator notes, `docs/configuration.md` for config keys, `docs/ffi.md` for the C API.
- Style: short sentences, simple words, present tense, imperative where it instructs. State what and why, then stop. No hype words, no emoji. No em dashes or en dashes as punctuation; use periods, commas, or parentheses. Hyphens inside compound words are fine.
- Keep the README compact and in its current section order. Details belong in `docs/`.
- Prefer tables for options and formats. State limits in one honest sentence instead of hiding them.

## Working with git worktrees

If you use worktrees for parallel branches, share one `target` directory (symlink it or set `CARGO_TARGET_DIR`). Cargo judges freshness by mtime and workspace-relative path, so artifacts built from one worktree look fresh to another. After building elsewhere, run `find crates -name '*.rs' -exec touch {} +` in the worktree you are testing, or `cargo clean -p` the workspace crates. Never run cargo in two worktrees against the same target at once.

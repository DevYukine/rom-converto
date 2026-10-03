# Contributing to rom-converto

Thanks for helping. Bug reports, format requests, documentation fixes, and code are all welcome. This page covers the basics. The full code and review rules live in [AGENTS.md](AGENTS.md). For a mistake in the docs or help text, open a [documentation issue](https://github.com/DevYukine/rom-converto/issues/new?template=documentation.yml) or fix it in a pull request.

## Report a bug

Open a [bug report](https://github.com/DevYukine/rom-converto/issues/new?template=bug_report.yml). The most useful reports contain:

- The exact command. Each page of the desktop app shows the equivalent CLI command once a file is staged; copy it from there.
- The version from `rom-converto --version` or the app title bar.
- The error text. For the CLI, rerun with `-vv` and paste the output, or write a log with `--debug-log log.txt` and attach it. For batch runs, attach a `--report report.json` where the command supports it.
- The console and format involved, and whether the same file works in another tool.

Do not attach ROMs, keys, or other copyrighted files. Describe them (size, format, region) or name the DAT entry instead. Logs contain local paths; replace anything private before posting.

## Ask for a feature or a format

Open a [feature request](https://github.com/DevYukine/rom-converto/issues/new?template=feature_request.yml) or a [format request](https://github.com/DevYukine/rom-converto/issues/new?template=format_request.yml). Say which problem it solves and which console it affects. For a format, link a spec or a reference implementation if one exists; that usually decides how fast it can be done.

For usage questions, read the [docs](docs/) first, then open a [question](https://github.com/DevYukine/rom-converto/issues/new?template=question.yml). Check the [existing issues](https://github.com/DevYukine/rom-converto/issues) before filing anything. A comment or a reaction on an open issue beats a duplicate.

## Set up a development build

Follow the [development guide](docs/development.md) for the requirements, including the Tauri system libraries that the desktop app crate needs. Fork the repository on GitHub, then:

```sh
git clone https://github.com/<your-user>/rom-converto.git
cd rom-converto
git remote add upstream https://github.com/DevYukine/rom-converto.git
git fetch upstream
git switch -c develop upstream/develop
cargo build -p rom-converto-cli
```

For the desktop app, run `pnpm install --frozen-lockfile` and `pnpm tauri dev` in `crates/rom-converto-gui`.

## Make a change

1. Branch from `develop`. `main` only receives release commits.
2. Name the branch after the change type: `feat/<slug>`, `fix/<slug>`, `perf/<slug>`, `docs/<slug>`, `chore/<slug>`.
3. Keep the pull request to one change. Refactors, reformatting, and unrelated fixes go in their own PR.
4. Match the surrounding code. Rust is formatted with `cargo fmt`. Frontend files keep the indentation of the file you edit.
5. Add or update tests for the behaviour you changed. Do not commit ROMs, keys, or large binaries.
6. Update the documentation under `docs/` when you add or change an option, a format, or a GUI page.
7. Open the pull request against `develop`. GitHub preselects `main`; change it.

Larger changes, such as a new console or a new output format, are easier to review when you open an issue first and describe the plan.

## Run the checks

CI builds with the current stable toolchain, so run `rustup update stable` first. Then run the commands listed under [Checks](docs/development.md#checks) in the development guide. They are the same commands CI runs: `cargo fmt`, `cargo check`, `cargo test`, `cargo clippy` with `-D warnings`, the TypeScript binding check, and `pnpm test` plus `pnpm typecheck` for the frontend.

Two things CI checks that are easy to miss:

- If you changed Rust types the GUI uses, or the CLI echo tables in `runner/cli_echo.rs`, regenerate the bindings with `cargo test -p rom-converto-gui --features ts-export ts_export` and commit the result.
- Code behind `cfg(windows)` or `cfg(target_os = "macos")` only compiles in the build workflows, not in the test job. Check it on that platform or say that you could not.

Format and conversion code should also be checked against real files on your machine. Round trips that reproduce the input byte for byte, or outputs that load in the target emulator, are the usual proof. Mention what you verified in the PR.

Workflow runs on pull requests from forks start after a maintainer approves them.

## Commit messages and PR titles

Pull requests are squash-merged into `develop`. The squash commit takes its subject from the PR title, or from the commit subject when the PR has a single commit. For `feat`, `fix`, and `perf` that subject becomes a changelog line. Write commit subjects and the PR title as one-line [Conventional Commits](https://www.conventionalcommits.org/):

```
fix(chd): extract CD images to one bin per track
feat(nx): decrypt nsp and xci for nxemu
docs(cli): align organize help with sibling commands
```

- Start with a lowercase verb and describe the change itself. Do not name another tool as the reference ("like Dolphin", "match nsz"); the changelog has to read on its own.
- The type and scope lists are in [AGENTS.md](AGENTS.md#commit-messages).
- Do not edit `CHANGELOG.md` or the version in `Cargo.toml`. The release workflow does that.

The PR description is a short summary of what changed and why, plus `Closes #N` when it fixes an issue. For visual changes, add before and after screenshots. No checklists.

## Review

The maintainer reviews every pull request. Expect questions about edge cases, memory use on large files, and behaviour on Windows, macOS, and Linux. Small follow-up fixes may be pushed onto your branch before merging, so keep "Allow edits by maintainers" enabled.

## License

rom-converto is released under the [MIT License](LICENSE). By contributing you agree that your contribution is licensed under the same terms.

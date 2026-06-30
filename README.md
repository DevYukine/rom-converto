# rom-converto

A utility suite for converting, compressing, encrypting, and decrypting ROMs across **Nintendo 3DS**, **GameCube**, **Wii**, **Wii U**, **Nintendo Switch**, and **CD image** formats.

Available as both a **command line tool** and a **desktop GUI application**.

Built for developers, tinkerers and archivists.

## Features

### Nintendo 3DS (CTR)

* [x] Convert CDN files to `.cia`
* [x] Generate tickets for CDN files
* [x] Decrypt `.cia`, `.3ds`, `.cci`, and `.cxi` for emulator use (e.g. [Azahar](https://azahar-emu.org/)), streaming in bounded chunks without temporary files
* [x] Compress and decompress ROMs using the Z3DS format (seekable zstd)
* [x] Verify `.cia` legitimacy and `.3ds` / `.cci` NCCH partition integrity
* [x] See [`benchmark/3DS.md`](benchmark/3DS.md) for performance numbers

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

### Wii U (WUP)

* [x] Bundle titles, updates, and DLC into Cemu's `.wua` archive format
* [x] Decrypt NUS-format titles (`title.tmd` + `title.tik` + `.app` files) into a loadiine-shaped `meta/code/content` tree
* [x] Also accepts the community layout variant (`tmd.<N>` + optional `cetk.<N>` + extensionless content files)
* [x] Wii U disc images (`.wud` / `.wux`) accepted as `.wua` input, with per-disc key files
* [x] FST-aware inherited-file skipping so update overlays install cleanly on top of the base title

### Nintendo Switch (NX)

* [x] Compress `.nsp` to `.nsz` and `.xci` to `.xcz` using zstd inside the NCZ format
* [x] Decompress `.nsz` / `.xcz` back to raw `.nsp` / `.xci`
* [x] Both solid mode (one zstd frame per NCA) and block mode (random-read friendly fixed-size frames)
* [x] Verify per-NCA hash integrity on any container (`.nsp` / `.nsz` / `.xci` / `.xcz`)
* [x] Drop-in replacement for [`nsz`](https://github.com/nicoboss/nsz) with byte-identical output and matching CLI defaults
* [x] See [`benchmark/Switch.md`](benchmark/Switch.md) for performance numbers

### CD / DVD images (CHD / CUE+BIN)

* [x] Compress `.bin` + `.cue` pairs to CD-mode `.chd`
* [x] Compress CD-media `.iso` (PS1, PS2-CD) to CD-mode `.chd` with a single MODE1/2048 track (the `chdman createcd` equivalent)
* [x] Compress PS2-DVD / PSP `.iso` to DVD-mode `.chd` (the `chdman createdvd` equivalent); the CD/DVD media type is probed from the image, so the createcd vs createdvd mixup cannot happen
* [x] Compatibility-first DVD codecs (`lzma` + `zlib`) that load everywhere including AetherSX2/NetherSX2; opt-in `--zstd` for modern emulators
* [x] PSP images get 2048-byte hunks automatically (what PPSSPP expects); PS2 images use the chdman default
* [x] Extract `.chd` back to `.bin` + `.cue` (CD) or `.iso` (DVD), auto-detected
* [x] Verify `.chd` integrity via SHA-1 checksums, with optional header repair
* [x] Read chdman-produced DVD CHDs including `huff` and `flac` compressed hunks
* [x] Merge a multi-bin `.cue` (one `.bin` per track) into a single `.bin` + `.cue` pair, for emulators that cannot load split images
* [x] See [`benchmark/CHD.md`](benchmark/CHD.md) for performance numbers

### PSP / PS2 compressed ISOs (CSO / ZSO)

* [x] Compress `.iso` to `.cso` (CISO v1) for real PSP hardware with CFW and for PPSSPP
* [x] Compress `.iso` to `.zso` (LZ4) for real PS2 hardware via Open PS2 Loader 1.2+ and ARK-4 on PSP
* [x] Decompress `.cso` / `.zso` back to the original `.iso`
* [x] Verify container structure, with an optional full decode pass
* [x] maxcso-compatible defaults: 2 KiB blocks (16 KiB for 2 GiB+ inputs), automatic index shift for large images, per-block store-raw fallback

Where each format works:

| Target | Recommended format |
|---|---|
| PCSX2 / NetherSX2 (PS2 emulators) | DVD-mode CHD (default codecs) |
| PPSSPP (PSP emulator) | CHD or CSO |
| Real PSP with CFW | CSO |
| Real PS2 via Open PS2 Loader | ZSO |

### Application

* [x] Command line interface with progress bars and a post-run space-saved summary
* [x] Global `--dry-run` preview that shows the planned actions without writing anything
* [x] Three-step verbosity ladder: `-v` (debug), `-vv` (trace), `-vvv` (trace including dependency logs)
* [x] Global `--debug-log <FILE>` flag that writes a full trace log to a file independently of the console verbosity
* [x] Pre-run free-space check that aborts a write-producing run before it starts when the output filesystem looks too full; `--skip-space-check` bypasses it
* [x] OS and NAS junk files and directories (.DS_Store, AppleDouble "._" sidecars, @eaDir, $RECYCLE.BIN, Thumbs.db, and similar) are skipped automatically during recursive scans, so they are never treated as ROM inputs
* [x] `--on-conflict overwrite-invalid` mode that verifies existing outputs and rewrites only the broken or missing ones
* [x] Desktop GUI with drag and drop batch processing
* [x] Standalone `hash` command for crc32 / sha1 / md5 / sha256 digests with report export
* [x] `playlist` command to generate `.m3u` files for multi-disc sets (PS1, PS2, Saturn, Dreamcast) from filenames
* [x] Self update from GitHub releases (CLI)

## GUI Application

The desktop app provides a visual interface for all operations. Built with [Tauri](https://tauri.app/), [Nuxt](https://nuxt.com/) and [Tailwind CSS](https://tailwindcss.com/).

**Highlights:**

* Drag and drop files directly into the application
* Batch process multiple files at once (drop several files to queue them up)
* Real time progress tracking for all operations
* A Cancel button that aborts the running conversion immediately, both for single files and within a batch, and discards the partial output it created. Cancellation covers every decrypt, encrypt, compress, and decompress operation across all consoles, with none skipped: RVZ, .wua compress and NUS decrypt, z3ds compress and decompress, 3DS decrypt, CIA/CCI conversion, CDN to CIA, NSZ/XCZ, CHD, and CSO/ZSO. A file chosen for overwrite is left untouched. A cancelled run is reported with its own distinct status rather than as a failure.
* State persists when switching between pages
* Works on Windows, macOS and Linux

### CLI / GUI parity

The GUI surfaces every meaningful CLI capability. The table below maps each CLI command to its GUI page.

| CLI command | GUI page | Notes |
|---|---|---|
| `chd compress` / `extract` / `verify` / `info` | CD / DVD (CHD) | conflict policy on compress, including overwrite-invalid; recursive folder scan on compress and extract |
| `cso compress` / `decompress` / `verify` / `info` | PSP / PS2 (CSO/ZSO) | conflict policy on compress and decompress, including overwrite-invalid; recursive folder scan |
| `ctr` (cdn-to-cia, decrypt, compress, decompress, convert, verify, generate-ticket, info) | 3DS | conflict policy on every write command, including overwrite-invalid; recursive folder scan on decrypt, compress, decompress, and convert |
| `dol compress` / `decompress` / `verify` / `info` | GameCube | conflict policy on compress and decompress, including overwrite-invalid; recursive folder scan |
| `rvl compress` / `decompress` / `verify` / `info` | Wii | conflict policy on compress and decompress, including overwrite-invalid; recursive folder scan |
| `wup compress` / `decrypt` / `verify` / `info` | Wii U | conflict policy on compress and decrypt, including overwrite-invalid |
| `nx compress` / `decompress` / `verify` / `info` | Switch | conflict policy on compress and decompress, including overwrite-invalid; recursive folder scan |
| `cue merge` | CD (CUE/BIN) | full conflict policy |
| `hash` | Utilities -> Hash | CRC32/SHA1/MD5/SHA256, recursive folder scan |
| `playlist` | Utilities -> Playlist | .m3u generation, conflict policy |

Every GUI control forwards to the same library function the CLI uses, so a GUI run and the equivalent CLI command produce identical output. The CLI command echo above each result reflects the chosen options.

**Conflict policy.** Pages that write output expose an "On conflict" control with Overwrite, Skip, Rename, Error, and Overwrite if invalid, replacing the older force toggle. The choice is resolved before the write so Skip and Error never touch an existing file. With Overwrite if invalid the GUI runs the same per-format integrity verify the CLI does before deciding keep versus rewrite, and falls back to existence-based skip for outputs that have no integrity check, matching the CLI.

**Recursive folder scan.** Dropping or browsing a folder onto a write-capable batch page scans it for matching input files and queues them, using the same junk-filtered library walk as the CLI. A Recursive toggle with an optional max-depth controls how deep the scan goes, defaulting to full depth like the CLI `-R`.

**Disk space preflight.** Before any write-producing command runs, the GUI checks the output filesystem for free space, using the total size of the input files as a conservative floor plus a 256 MiB headroom. If space looks insufficient it reports an error and writes nothing, naming the directory, the estimated need, and the space available. The estimate is a floor and cannot account for decompression that expands well beyond its input, so the value is in catching a near-full disk before a long batch starts. If the free-space query fails, the run proceeds. The "Skip free space check" toggle on each write page bypasses the guard, matching the CLI's `--skip-space-check`.

**GUI to CLI direction.** The GUI adds no conversion capability the CLI lacks. Info caching, drag and drop, the batch queue, and the CLI command echo are interface conveniences over the same library calls, so no CLI changes were needed.

**Intentional non-parity.** These CLI features have no GUI counterpart by design:

* `shell-completions`: shell integration for a terminal, not the desktop app.
* `self-update`: the desktop app updates itself through Tauri.
* `-v/--verbose`, `--debug-log`, `-q/--quiet`: terminal logging controls; the GUI shows operation output in its own log panel.
* `--config`, `--preset`, `--no-update-check`: file based configuration and updater flags; the GUI keeps options per page.
* `info --json`: scripting output for the terminal; the GUI shows a rich info card.

**Run reports and output templates.** The GUI exposes `--report` on the compress and decompress pages for every format (dol, rvl, cso, nx) plus chd compress and chd extract. It accumulates one record per processed file and calls the same `write_report` library function the CLI uses, so the CSV/JSON/HTML output is identical. The report file is overwritten directly and does not go through the on-conflict control, matching the CLI. The GUI also exposes `--output-template` on those pages plus the four CTR operations (compress, decompress, convert, decrypt). For CTR the template is single file only, so the field is hidden once files are queued for a batch. The template resolves through the same `TemplateTokens` and `apply_template` library functions, so the resolved path matches the CLI. Extract report rows carry zero byte sizes, the same as the CLI, since extraction writes several files.

**Option gating.** The GUI disables options that do not apply to the current selection and explains why with a tooltip. An output template and an explicit output path are mutually exclusive, so entering a template disables the explicit output field with a note that the template controls the output path. On the Wii U decrypt page the Rename conflict policy is disabled because the output is a directory. The Run button is disabled, with a tooltip giving the exact reason, whenever the configuration cannot run, for example when no input is selected, when a queue is empty, or when an output template field is set but blank.

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

All commands that write an output file use `--on-conflict` to decide what happens when the output already exists. The choices are `error` (the default, refuse and stop), `overwrite` (replace the existing output), `skip` (leave the existing output and move on, reported as skipped in the summary), `rename` (write to the next free numbered sibling, for example `Game.chd` becomes `Game (1).chd`), and `overwrite-invalid` (verify the existing output; if it passes, keep it and report it as skipped; if it fails verification or cannot be verified, rewrite it; a missing output is always written). `overwrite-invalid` is useful when re-running after a partial failure or a settings upgrade, since only outputs that are missing or broken get rewritten. `-f`/`--force` is a shorthand for `--on-conflict overwrite` and cannot be combined with `--on-conflict`. For `wup decrypt` the output is a directory, so `rename` is not supported there and falls back to `error`. For `chd extract` and `cue merge`, which write more than one file, the policy applies to the base output path and the sidecars follow it.

With `overwrite-invalid`, each existing output is checked for integrity before the decision is made. The check is skipped when the output does not exist. It runs the same per-format verification as the `verify` command. `chd compress` and `cso compress` check the `.chd` or `.cso` they would produce, reading the file in full, which costs about as much as a decompression pass on a large file. `dol compress` and `rvl compress` verify the `.rvz` they would produce against its stored SHA-1 structural hashes, which is fast and does not decompress any group data. `nx compress` verifies the `.nsz`/`.xcz` by decrypting every NCA section and checking the stored hash hierarchy; this requires `prod.keys` and costs about as much as the compress pass itself, and when keys are unavailable or incomplete it falls back to existence-based skip rather than rewriting a file it cannot verify. For operations whose output has no integrity check, such as the raw `.iso` from `cso decompress`, `chd extract`, `rvz decompress`, or the raw `.nsp`/`.xci` from `nx decompress`, `overwrite-invalid` falls back to existence-based skip: an existing output is kept and a missing one is written. Recursive `ctr` commands manage their own output paths and behave the same way under this policy.

After `compress`, `decompress`, and `convert` operations, the tool prints a closing summary of bytes processed and space saved or expanded, for example `12 files: 12.4 GiB -> 4.1 GiB, saved 8.3 GiB (67%) in 2m14s`. Verify and extract operations print a file count and elapsed time instead.

Pressing Ctrl-C stops the current operation cleanly. The running conversion aborts mid-file at the next safe point rather than running to completion, the partial output the operation created is removed, a `Cancelled.` message is printed, and the process exits with code 130. This covers every decrypt, encrypt, compress, and decompress operation across all consoles: GameCube and Wii RVZ, Wii U .wua compress and NUS decrypt, 3DS z3ds compress and decompress, 3DS decrypt, CIA/CCI conversion, CDN to CIA, Switch NSZ/XCZ, CHD, and CSO/ZSO. None are skipped. The only steps without a cancellation checkpoint are single fixed-size writes with no data loop, such as generating a ticket file, where there is nothing to interrupt mid-stream. A pre-existing file chosen for overwrite is left untouched, since the conversion writes to a temporary sibling file and only renames it into place once it finishes. In a batch run, including the recursive `ctr decrypt` and `ctr convert` directory walks, the file in progress is cancelled and the loop stops; files already converted are kept.

The `compress`, `decompress`, and `chd extract` commands also accept `--report <FILE>` to write a structured run report after the run. The format is inferred from the file extension and the numbers match the closing summary. The report file is overwritten directly and does not go through `--on-conflict`, since it is an output you named explicitly rather than a converted ROM. See [Run reports](#run-reports) for the formats and schema.

Two global flags work on every command: `--config <FILE>` points at a config file directly and overrides the search order, and `--preset <NAME>` applies a named preset from the config. See [Configuration](#configuration) for the file format and how settings are resolved.

Console verbosity is a three-step ladder. `-v` shows debug-level messages from the rom-converto modules, `-vv` raises them to trace level, and `-vvv` shows trace-level output from every module including dependencies. `--quiet` suppresses everything except warnings and errors and takes precedence over `-v`. Separately, `--debug-log <FILE>` writes a full trace log (every module at trace level, with timestamps and module targets) to FILE for the current run, regardless of the console verbosity. The file is created fresh at startup and is useful for attaching a complete log to a bug report without flooding the terminal; if it cannot be opened the command stops with an error before doing any work.

The compress, decompress, convert, decrypt, and `chd extract` commands also accept `--output-template`, an alternative way to derive the output path from the ROM's own metadata. See [Output-path templates](#output-path-templates).

`--dry-run` is a global flag that previews what a command would do without writing any output. It prints one plan line per file showing the operation, the resolved and templated output path, the `--on-conflict` decision (`overwrite`, `rename`, `skip`, or `new`), the detected media or format, and any missing keys, for example `would compress game.iso -> game.cso (CSO) [overwrite]`. Under `--on-conflict overwrite-invalid` the verify is read-only, so the preview runs it and shows `[keep (valid)]` or `[rewrite (invalid)]` for an existing output. It runs the same input resolution, detection, and conflict checks as a real run, exits 0 on a valid plan, and exits nonzero only for real input errors such as a missing file. Pass `--report` alongside it to export the plan. For the recursive `ctr` file batches (`decrypt`, `compress`, `decompress`, `verify`) the preview lists resolved output paths only, since those batches do not expose a per-file conflict policy. Recursive `cdn-to-cia` does honor `--on-conflict`, so its preview shows the decision per produced `.cia`.

Before any write-producing operation, the CLI estimates how much space the outputs need, using the total size of the input files as a conservative floor, and checks the free space on the output filesystem. If there is not enough room it aborts before writing anything, naming the directory, the estimated need, and the space available. This is a best-effort check, not a guarantee: it cannot know exact output sizes, and decompression in particular can produce far more than the compressed input, so the estimate is a floor. The value is catching a near-full disk before a long batch starts rather than minutes in. If the free-space query fails, for example on an unsupported filesystem, the check is skipped and the run proceeds. Under `--dry-run` nothing is written, so the check never aborts. Pass `--skip-space-check` to disable the preflight entirely.

The GUI surfaces `--dry-run` as a Preview toggle on nearly every write-capable page. Turning it on makes the Run button preview the plan instead of running it: one plan line per file in the same `would <op> <in> -> <out> (<FMT>) [<decision>]` form as the CLI, with nothing written. The preview shares the CLI's plan logic through the library, so a GUI preview line matches the CLI's `--dry-run` line for the same input, including the read-only `overwrite-invalid` verify that distinguishes `[keep (valid)]` from `[rewrite (invalid)]`. Recursive `cdn-to-cia` is the one write page without a preview, mirroring how the CLI special-cases that batch.

## Configuration

A TOML config file lets you set per-format default flags and named presets so you do not have to repeat long flag combinations. The config is optional: with no config file the built-in defaults apply.

### Search order

The first existing file in this order is used:

1. The path given to `--config <FILE>`. If you pass `--config` and the file does not exist, the command stops with an error.
2. `./rom-converto.toml` in the current directory.
3. `./.rom-converto.toml` in the current directory.
4. The per-user config directory:
   - Linux: `~/.config/rom-converto/config.toml`
   - macOS: `~/Library/Application Support/rom-converto/config.toml`
   - Windows: `%APPDATA%\rom-converto\config.toml`

### Precedence

For each setting, the value is resolved in this order, highest first:

1. An explicit flag on the command line.
2. The selected `--preset` value for that format.
3. The config file format default for that format.
4. The built-in default.

An unset flag never overrides a preset or config value. For example, if a preset sets `level = 22` and you run a compress without `-l`, the level is 22; passing `-l 5` uses 5.

### Behavior

- A missing config file is not an error: the built-in defaults apply.
- A malformed config file is a hard error that names the file, so a typo is not silently ignored. Unknown keys are rejected the same way.
- An unknown `--preset` name stops with an error that lists the available preset names.
- Relative `output_dir` and `report` paths resolve against the directory that holds the config file, not the current directory.

### Covered settings

The config covers the tuning knobs that are worth repeating: `level`, `chunk_size`, `block_size_exp`, `mode`, `hunk_size`, `block_size`, `on_conflict`, `output_dir`, and `report`, each under the matching format table.

Some flags are deliberately command-line only and are not read from the config: `--recursive` and `--max-depth` (they change how much of a directory tree is processed), `--output-template` (it changes which output file is written, like the above), the `cso` `--format`, and the `chd` `--dvd`/`--cd`/`--zstd` selectors (they change which output container or codec set you produce). Keeping these out of the config avoids silently changing what gets traversed or what file is written.

### Example

```toml
[dol]
level = 22
chunk_size = 131072
on_conflict = "skip"
output_dir = "./rvz"

[nx]
level = 18
mode = "solid"
block_size_exp = 20

[chd]
hunk_size = 4096

[presets.archive]
dol = { level = 22, chunk_size = 131072 }
nx = { level = 22, mode = "solid" }

[presets.fast]
dol = { level = 5 }
nx = { level = 10 }
```

Run a preset with `rom-converto dol compress game.iso --preset archive`.

The `mode` key for `nx` is the NX codec mode (`solid` or `block`, the same as `nx --mode`); it is unrelated to these named presets, which are bundles of settings you choose by name.

The GUI does not yet read this config file. The CLI is the supported path for now; GUI support is a planned follow-up.

### CTR (Nintendo 3DS)

| Command | Description |
|---|---|
| `ctr cdn-to-cia <CDN_DIR> [OUTPUT]` | Convert a CDN directory to `.cia` |
| `ctr generate-cdn-ticket <CDN_DIR> [OUTPUT]` | Generate a `.tik` ticket from CDN content |
| `ctr decrypt <INPUT> [OUTPUT]` | Decrypt an encrypted ROM for emulator use |
| `ctr compress <INPUT> [OUTPUT]` | Compress a decrypted ROM to Z3DS format |
| `ctr decompress <INPUT> [OUTPUT]` | Decompress a Z3DS file back to the original ROM |
| `ctr convert <INPUT> [OUTPUT]` | Convert between `.cia` and `.cci`/`.3ds`, direction auto-detected |
| `ctr verify <INPUT>` | Verify `.cia` legitimacy or `.3ds`/`.cci` NCCH integrity |
| `ctr info <INPUT>` | Inspect 3DS metadata; see the Info section |

**`cdn-to-cia` flags:**

| Flag | Description |
|---|---|
| `-C, --cleanup` | Remove original CDN files after conversion |
| `-R, --recursive` | Convert each immediate child directory of CDN_DIR to a `.cia`. This scans the direct subdirectories, unlike the file-recursive subcommands below, so it takes no `--max-depth`. Each per-title output honors `--on-conflict`, so an existing `.cia` is refused, skipped, renamed, or overwritten per the policy instead of being replaced unconditionally |
| `-T, --ensure-ticket-exists` | Auto-generate a ticket file if one is not found |
| `-D, --decrypt` | Also decrypt the CIA after creation |
| `-Z, --compress` | Also compress the CIA after creation (implies decrypt) |
| `--on-conflict <POLICY>` | What to do when the output exists: `error` (default), `overwrite`, `skip`, `rename`, or `overwrite-invalid` (a CIA has no cheap integrity check, so this falls back to existence-based skip) |
| `-f, --force` | Alias for `--on-conflict overwrite` |

**`decrypt` / `compress` / `decompress` / `convert` flags:**

| Flag | Description |
|---|---|
| `-o, --output <FILE>` | Output path (alternative to the positional OUTPUT argument) |
| `--output-template <TEMPLATE>` | Build the output path from metadata tokens. Single-file runs only for CTR. See [Output-path templates](#output-path-templates) |
| `-R, --recursive` | Recursively process every matching file in INPUT and its subdirectories |
| `--max-depth <N>` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |
| `--on-conflict <POLICY>` | What to do when the output exists: `error` (default), `overwrite`, `skip`, or `rename` |
| `-f, --force` | Alias for `--on-conflict overwrite` |
| `--allow-encrypted` | Compress even if the input ROM appears encrypted. `compress` only. By default an encrypted ROM is refused, since encrypted 3DS content has a near-zero compression ratio. Decrypt first with `ctr decrypt`, or pass this to force it |

**`verify` flags:**

| Flag | Description |
|---|---|
| `--full` | Also verify content hashes against the TMD (CIA only, slower). `--verify-content` is a visible alias for this flag |
| `-R, --recursive` | Recursively verify every matching file in INPUT and its subdirectories and print a summary |
| `--max-depth <N>` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |

> **`generate-cdn-ticket`:** Generated tickets use placeholder values (null Console ID, etc.) and only work on modded consoles and emulators. They will not work on stock hardware.

> **`decrypt`:** Supports `.cia`, `.3ds`, `.cci` and `.cxi` files. The format is detected automatically. Place a `seeddb.bin` file next to the executable to resolve seeds locally. If none is found, the tool will fetch the required seed from Nintendo's API.

> **`compress` / `decompress`:** Supported input formats for compression: `.cia`, `.cci`, `.3ds`, `.cxi`, `.3dsx`. Output files use the Z3DS format (`.zcia`, `.zcci`, `.zcxi`, `.z3dsx`). Compression only works on decrypted ROMs, since encrypted ROMs have near-zero compression ratios. `compress` enforces this: it inspects the NCCH/NCSD/CIA crypto flags and refuses an input that still looks encrypted, pointing you to `rom-converto ctr decrypt <INPUT>` and writing no output. A header it cannot parse is also refused, to avoid wasting a full compression pass on a file whose state is unknown. Pass `--allow-encrypted` to override and compress anyway, which prints a warning. In a recursive run each refused file is logged and skipped while the rest of the batch continues. `.3dsx` homebrew has no encryption and is never checked. The output file path defaults to the input path with the extension updated automatically.

> **`convert`:** Direction is auto-detected from the INPUT extension: `.cia` becomes `.3ds` (CCI/NCSD), and `.3ds`/`.cci` become `.cia`. CIA output is unsigned with a zero title key; compatible with CFW (Luma3DS) and emulators, but not installable on stock 3DS.

> **`verify`:** Exits nonzero on verification failure.

---

### DOL (GameCube)

| Command | Description |
|---|---|
| `dol compress <INPUT> [OUTPUT]` | Compress a `.iso`/`.gcm` to Dolphin's `.rvz` |
| `dol decompress <INPUT> [OUTPUT]` | Decompress a `.rvz` back to `.iso` |
| `dol verify <INPUT>` | Verify RVZ container hashes, or compute a whole-disc SHA-1 with `--full` |
| `dol info <INPUT>` | Inspect GameCube disc metadata; see the Info section |

**Flags:**

| Flag | Applies to | Description |
|---|---|---|
| `-l, --level <LEVEL>` | `compress` | Zstandard compression level (signed, defaults to 22, Dolphin's max non-extreme) |
| `--chunk-size <BYTES>` | `compress` | Chunk size in bytes, power of two between 32 KiB and 2 MiB (defaults to 128 KiB to match Dolphin) |
| `-o, --output <FILE>` | `compress`, `decompress` | Output path (alternative to the positional OUTPUT argument) |
| `--output-template <TEMPLATE>` | `compress`, `decompress` | Build the output path from metadata tokens. See [Output-path templates](#output-path-templates) |
| `--on-conflict <POLICY>` | `compress`, `decompress` | What to do when the output exists: `error` (default), `overwrite`, `skip`, `rename`, or `overwrite-invalid` (verifies the `.rvz` structural hashes on `compress`, fast and without decompression; `decompress` writes a raw `.iso` with no integrity check and falls back to existence-based skip) |
| `-f, --force` | `compress`, `decompress` | Alias for `--on-conflict overwrite` |
| `-R, --recursive` | `compress`, `decompress`, `verify` | Recursively process every matching file in INPUT and its subdirectories |
| `--max-depth <N>` | `compress`, `decompress`, `verify` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |
| `--report <FILE>` | `compress`, `decompress` | Write a run report to FILE. Format inferred from the extension: `.csv`, `.json`, `.html` or `.htm`. Unknown extensions default to JSON. Covers every processed file with paths, status, byte sizes, space-saved ratio, elapsed time, and any error. Overwritten directly, ignoring `--on-conflict` |
| `--full` | `verify` | Decode the whole disc and compute a whole-disc SHA-1 |

---

### RVL (Wii)

| Command | Description |
|---|---|
| `rvl compress <INPUT> [OUTPUT]` | Compress a `.iso`/`.wbfs` to Dolphin's `.rvz` |
| `rvl decompress <INPUT> [OUTPUT]` | Decompress a `.rvz` back to `.iso` |
| `rvl verify <INPUT>` | Verify RVZ container hashes, or recompute the Wii partition hash tree with `--full` |
| `rvl info <INPUT>` | Inspect Wii disc metadata; see the Info section |

Flags match the `dol` commands. `--full` on `rvl verify` decrypts every partition cluster and recomputes the H0/H1/H2 hash tree.

> `dol` and `rvl` share one RVZ pipeline. Output files are byte identical to Dolphin's own encoder and decoder in both directions, on both GameCube and Wii. See the [Benchmarks](#benchmarks) section for measured performance.

---

### WUP (Wii U)

| Command | Description |
|---|---|
| `wup compress -o <OUTPUT> <INPUTS>...` | Bundle one or more titles into a Cemu `.wua` archive |
| `wup decrypt -o <OUTPUT> <INPUT>` | Decrypt a NUS directory into a loadiine `meta/code/content` tree |
| `wup verify <INPUT>` | Verify Wii U content SHA-1 against the TMD |
| `wup info <PATH>` | Inspect Wii U title metadata; see the Info section |

**`compress` flags:**

| Flag | Description |
|---|---|
| `-o, --output <FILE>` | Output `.wua` file path |
| `-l, --level <LEVEL>` | Zstd compression level 0..=22 (0 = Cemu default of 6) |
| `--key <KEYFILE>` | Disc master key file. Pass once per disc input in positional order |
| `--on-conflict <POLICY>` | What to do when the output exists: `error` (default), `overwrite`, `skip`, or `rename` |
| `-f, --force` | Alias for `--on-conflict overwrite` |

**`decrypt` flags:**

| Flag | Description |
|---|---|
| `-o, --output <DIR>` | Output directory |
| `--on-conflict <POLICY>` | What to do when the output directory exists: `error` (default), `overwrite`, or `skip`; `rename` is not supported for directory outputs and falls back to `error` |
| `-f, --force` | Alias for `--on-conflict overwrite` |

**`verify` flags:**

| Flag | Description |
|---|---|
| `--key <KEYFILE>` | Disc master key file (`.wud`/`.wux` inputs only) |
| `-R, --recursive` | Recursively verify every `.wud` / `.wux` disc image in INPUT and its subdirectories. NUS title directories are detected among the immediate children of INPUT only |
| `--max-depth <N>` | Limit disc image recursion depth with `--recursive`. `1` = top level only. Default: unlimited. Does not affect NUS title directory discovery |

> **`compress`:** Each input is auto-detected as a loadiine directory, a NUS directory, or a disc image (`.wud` / `.wux`). Disc images need a 16-byte master key; keys are resolved in order from `--key` flags, a sibling `<disc>.key` file, or `game.key` in the same directory. Multiple titles (base + update + DLC) can be bundled into one archive and each lands under its own `<titleId>_v<version>/` folder, the layout Cemu expects.

> **`decrypt`:** Writes the decrypted tree to the output directory. Handles both the canonical Nintendo layout (`title.tmd` + `title.tik` + `{id}.app`) and the community layout variant (`tmd.<N>` + optional `cetk.<N>` + extensionless content files). When no ticket is present, the title key is derived from the title id via the Nintendo CDN's PBKDF2 scheme.

---

### NX (Nintendo Switch)

| Command | Description |
|---|---|
| `nx compress <INPUT> [-o OUTPUT]` | Compress a `.nsp` to `.nsz` or a `.xci` to `.xcz` |
| `nx decompress <INPUT> [-o OUTPUT]` | Decompress a `.nsz` / `.xcz` back to `.nsp` / `.xci` |
| `nx verify <INPUT>` | Verify per-NCA hash integrity of any Switch container |
| `nx info <INPUT>` | Inspect Switch container metadata; see the Info section |

**`compress` flags:**

| Flag | Description |
|---|---|
| `--keys <PRODKEYS>` | Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` (or `%USERPROFILE%/.switch/prod.keys` on Windows) |
| `-o, --output <FILE>` | Output path. Defaults to the input with the extension switched (`.nsp` -> `.nsz`, `.xci` -> `.xcz`) |
| `--output-template <TEMPLATE>` | Build the output path from metadata tokens. See [Output-path templates](#output-path-templates) |
| `-l, --level <LEVEL>` | Zstd compression level 1..=22 (defaults to 18, matching `nsz`) |
| `--mode <MODE>` | `solid` (one zstd frame per NCA, default for NSP) or `block` (independent zstd frames per fixed-size block, default for XCI) |
| `--block-size-exp <EXP>` | Block-mode block size as `1 << exp` bytes, range 14..=32 (defaults to 20 = 1 MiB, matching `nsz`) |
| `--on-conflict <POLICY>` | What to do when the output exists: `error` (default), `overwrite`, `skip`, `rename`, or `overwrite-invalid` (verifies the `.nsz`/`.xcz` NCA hash hierarchy on `compress`; requires `prod.keys` and falls back to existence-based skip when keys are absent) |
| `-f, --force` | Alias for `--on-conflict overwrite` |
| `-R, --recursive` | Compress every `.nsp` and `.xci` found in INPUT and its subdirectories |
| `--max-depth <N>` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |
| `--report <FILE>` | Write a run report to FILE. Format inferred from the extension: `.csv`, `.json`, `.html` or `.htm`. Unknown extensions default to JSON. Overwritten directly, ignoring `--on-conflict` |

**`decompress` flags:**

| Flag | Description |
|---|---|
| `--keys <PRODKEYS>` | Path to `prod.keys`. Same default as `compress` |
| `-o, --output <FILE>` | Output path. Defaults to the input with the extension switched (`.nsz` -> `.nsp`, `.xcz` -> `.xci`) |
| `--output-template <TEMPLATE>` | Build the output path from metadata tokens. See [Output-path templates](#output-path-templates) |
| `--on-conflict <POLICY>` | What to do when the output exists: `error` (default), `overwrite`, `skip`, or `rename` |
| `-f, --force` | Alias for `--on-conflict overwrite` |
| `-R, --recursive` | Decompress every `.nsz` and `.xcz` found in INPUT and its subdirectories |
| `--max-depth <N>` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |
| `--report <FILE>` | Write a run report to FILE. Format inferred from the extension: `.csv`, `.json`, `.html` or `.htm`. Unknown extensions default to JSON. Overwritten directly, ignoring `--on-conflict` |

**`verify` flags:**

| Flag | Description |
|---|---|
| `--keys <PRODKEYS>` | Path to `prod.keys`. Same default as `compress` |
| `-R, --recursive` | Verify every `.nsp`, `.nsz`, `.xci`, and `.xcz` found in INPUT and its subdirectories |
| `--max-depth <N>` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |

> **`compress` / `decompress`:** Outputs are byte identical to `nsz` / `nsz -D` at matching settings. `prod.keys` is required to derive the per-NCA section keys; the file is read but never modified. Tickets inside the container are kept as-is so installation on console still works.

> **`verify`:** Walks every NCA inside the container and checks the stored hash hierarchy (FS hashes for PFS0 sections, IVFC for RomFS sections). Works on already-compressed `.nsz` / `.xcz` without decompressing first. Exits nonzero on verification failure.

---

### CHD

| Command | Description |
|---|---|
| `chd compress <INPUT> [OUTPUT]` | Compress a `.cue` or `.iso` to `.chd`; CD vs DVD media is auto-detected (PS1/PS2-CD iso becomes CD-mode, PS2-DVD/PSP iso becomes DVD-mode) |
| `chd extract <INPUT> [OUTPUT]` | Extract a `.chd` file back to `.bin` + `.cue` (CD) or `.iso` (DVD) |
| `chd verify <INPUT>` | Verify the SHA1 integrity of a `.chd` file |
| `chd info <INPUT>` | Inspect CHD metadata; see the Info section |

**Flags:**

| Flag | Applies to | Description |
|---|---|---|
| `--on-conflict <POLICY>` | `compress`, `extract` | What to do when the output exists: `error` (default), `overwrite`, `skip`, `rename`, or `overwrite-invalid` (verifies the `.chd` on `compress`; `extract` writes a raw image with no integrity check and falls back to existence-based skip) |
| `-f, --force` | `compress`, `extract` | Alias for `--on-conflict overwrite` |
| `--dvd` / `--cd` | `compress` | Override the auto-detected mode (CD mode needs a cue sheet) |
| `--hunk-size <BYTES>` | `compress` | DVD hunk size, a multiple of 2048; defaults to 4096, or 2048 for detected PSP images |
| `--zstd` | `compress` | Add zstd to the DVD codec set; better ratio, but rejected by AetherSX2/NetherSX2 |
| `-o, --output <FILE>` | `compress`, `extract` | Output path (alternative to the positional OUTPUT argument) |
| `--output-template <TEMPLATE>` | `compress`, `extract` | Build the output path from metadata tokens. For `extract` the sidecars share the resolved stem. See [Output-path templates](#output-path-templates) |
| `-R, --recursive` | `compress`, `extract`, `verify` | Recursively process every matching file in INPUT and its subdirectories |
| `--max-depth <N>` | `compress`, `extract`, `verify` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |
| `--report <FILE>` | `compress`, `extract` | Write a run report to FILE. Format inferred from the extension: `.csv`, `.json`, `.html` or `.htm`. Unknown extensions default to JSON. Overwritten directly, ignoring `--on-conflict`. Extract rows carry zero byte sizes since extraction writes several files |
| `-p, --parent <PARENT>` | `extract`, `verify` | Specify a parent CHD for parent-child relationships |
| `--fix` | `verify` | Correct SHA1 values in the CHD header if mismatches are found |

---

### CSO / ZSO

| Command | Description |
|---|---|
| `cso compress <INPUT> [OUTPUT]` | Compress an `.iso` to `.cso` (default) or `.zso` |
| `cso decompress <INPUT> [OUTPUT]` | Restore the original `.iso` from a `.cso` / `.zso` |
| `cso verify <INPUT>` | Validate the container index; `--full` decodes every block |
| `cso info <INPUT>` | Inspect CSO/ZSO metadata; see the Info section |

**Flags:**

| Flag | Applies to | Description |
|---|---|---|
| `--format <cso\|zso>` | `compress` | Output container: CSO for PSP/PPSSPP, ZSO for PS2 via OPL |
| `--block-size <BYTES>` | `compress` | Block size, a power of two; defaults to 2048 (16384 for 2 GiB+ inputs) |
| `-o, --output <FILE>` | `compress`, `decompress` | Output path (alternative to the positional OUTPUT argument) |
| `--output-template <TEMPLATE>` | `compress`, `decompress` | Build the output path from metadata tokens. See [Output-path templates](#output-path-templates) |
| `--on-conflict <POLICY>` | `compress`, `decompress` | What to do when the output exists: `error` (default), `overwrite`, `skip`, `rename`, or `overwrite-invalid` (verifies the `.cso`/`.zso` on `compress`; `decompress` writes a raw `.iso` with no integrity check and falls back to existence-based skip) |
| `-f, --force` | `compress`, `decompress` | Alias for `--on-conflict overwrite` |
| `-R, --recursive` | `compress`, `decompress`, `verify` | Recursively process every matching file in INPUT and its subdirectories |
| `--max-depth <N>` | `compress`, `decompress`, `verify` | Limit recursion depth with `--recursive`. `1` = top level only. Default: unlimited |
| `--report <FILE>` | `compress`, `decompress` | Write a run report to FILE. Format inferred from the extension: `.csv`, `.json`, `.html` or `.htm`. Unknown extensions default to JSON. Overwritten directly, ignoring `--on-conflict` |
| `--full` | `verify` | Decode every block instead of only checking the index |

---

### CUE/BIN

| Command | Description |
|---|---|
| `cue merge <INPUT_CUE> <OUTPUT_CUE>` | Merge a multi-bin `.cue` into a single `.bin` + `.cue` pair (the merged `.bin` is named after the output `.cue`) |

**Flags:**

| Flag | Applies to | Description |
|---|---|---|
| `--on-conflict <POLICY>` | `merge` | What to do when an output exists: `error` (default), `overwrite`, `skip`, or `rename`; the `.bin` sidecar follows the renamed `.cue` |
| `-f, --force` | `merge` | Alias for `--on-conflict overwrite` |

---

### Hash

```
rom-converto hash <INPUT> [--algo crc32,sha1,md5,sha256] [-R] [--max-depth N] [--report FILE]
```

Compute plain checksums for a file, or for every file in a directory with `-R`. This is a digest tool only: it reads the bytes and prints the hashes. It does no DAT or database lookup and compares nothing against any reference set.

```
rom-converto hash game.iso
rom-converto hash game.iso --algo sha1,sha256
rom-converto hash -R ./roms --report hashes.csv
```

| Flag | Description |
|---|---|
| `<INPUT>` | A file, or a directory with `-R` |
| `--algo <ALGOS>` | Comma-separated digests: `crc32`, `sha1`, `md5`, `sha256`. Default `crc32,sha1` |
| `-R`, `--recursive` | Hash every file under INPUT, descending into subdirectories |
| `--max-depth <N>` | Limit recursion depth when `-R` is set. 1 = top level only. Requires `-R` |
| `--report <FILE>` | Write a run report to FILE. Format inferred from the extension: `.csv`, `.json`, `.html` or `.htm`. Unknown extensions default to JSON. Overwritten directly |

All digests are computed in a single streaming pass per file, so memory stays constant no matter how large the input is. Hashes print as lowercase hex (crc32 is 8 hex characters). Any file type is accepted. Empty files produce the valid digests of empty input. Unreadable files are reported and skipped without aborting the batch. The report carries its own column schema (`path, crc32, sha1, md5, sha256, size_bytes, status, elapsed_ms, error`) with no size-ratio column, since hashing produces no output file.

---

### Playlist

```
rom-converto playlist <DIR> [--playlist-mode multiple|always] [--ext EXTS] [--output-dir DIR] [--max-depth N] [--on-conflict POLICY] [-f]
```

Scan a directory for disc images and write one `.m3u` per multi-disc game so emulators (RetroArch, Batocera, ES-DE, RetroPie) can swap discs. Grouping is filename-based only; no DAT lookup is done. The scan is recursive by default, so a whole library can be processed in one run.

```
rom-converto playlist ./roms
rom-converto playlist ./roms --playlist-mode always
rom-converto playlist ./roms --ext cue,chd --max-depth 2
```

| Flag | Description |
|---|---|
| `<DIR>` | Directory to scan for disc images |
| `--playlist-mode <MODE>` | `multiple` (default) writes an `.m3u` only for sets with more than one disc; `always` also writes a single-entry `.m3u` for single-disc games |
| `--ext <EXTS>` | Comma-separated disc image extensions to scan. Default `cue,chd,iso,cso,zso` |
| `--output-dir <DIR>` | Write the `.m3u` files here instead of beside the disc files |
| `--max-depth <N>` | Limit scan depth. 1 = top level only. Omit for unlimited |
| `--on-conflict <POLICY>`, `-f` | What to do when an `.m3u` already exists. `overwrite-invalid` has no integrity check for plain text, so it behaves as `skip` |

The grouping matches the Redump `(Disc N)` and `(Disc N of M)` conventions and the TOSEC `Disc N of M` and `(Disc N of M)` conventions. The base title is the filename with the disc token and its surrounding parentheses and whitespace removed; the `.m3u` is named after that base title. The match is case-insensitive on the word "Disc", and the word must be preceded by `(` or whitespace, so titles that genuinely contain it (`Discworld`, `Disco Elysium`) are never mis-grouped. When a name carries other parenthesized tags, only the last `(Disc N)` token is stripped, so a region tag like `(USA)` survives in the title.

Disc numbers are parsed as integers and sort numerically, so `Disc 2` precedes `Disc 10` and `Disc 01` equals `Disc 1`. Mixed extensions in one set are grouped together, so a set can list `Game (Disc 1).cue` and `Game (Disc 2).chd`. Entries are relative paths from the `.m3u` location and always use forward slashes, which is what frontends expect. When the discs of one set share a directory the `.m3u` is written next to them; when a set spans subdirectories the `.m3u` is written at the scan root with relative entries pointing into the subdirectories. Duplicate disc numbers are kept and a warning is printed. Output ordering is deterministic. With `--dry-run` the command prints which `.m3u` files would be written along with their full contents and writes nothing.

The GUI does not expose playlist generation yet; it is a planned follow-up.

---

### Output-path templates

`--output-template <STRING>` builds each output path from tokens filled by the metadata rom-converto already reads (the same data the `info` command shows). It is the easy way to turn a flat folder of ROMs into a sorted tree in one recursive run. It uses only in-tool metadata, never an external DAT.

```
rom-converto dol compress -R roms/ --output-dir organized/ --output-template "{console}/{title}.{ext}"
```

The template is a relative path. Tokens are written as `{name}` and any other text is kept literally, so `/` in the template creates a subdirectory. The resolved path is joined under `--output-dir` (or the current directory when no output directory is given).

| Token | Resolves to |
|---|---|
| `{title}` | 3DS SMDH short title, GameCube banner or header name, Wii IMET or header name, Wii U meta.xml long name, Switch NACP title. Prefers the English entry |
| `{titleId}` | 3DS title id, GameCube/Wii game id, Wii TMD title id (hex), Wii U title id (hex), Switch application id (hex) |
| `{region}` | 3DS SMDH region, GameCube/Wii region, Wii U region list. Empty for Switch and CHD/CSO |
| `{console}` | `3DS`, `GameCube`, `Wii`, `WiiU`, `Switch`, `CHD`, or `CSO` |
| `{serial}` | 3DS product code, GameCube/Wii game id, Wii U product code. Falls back to the basename otherwise |
| `{ext}` | The output extension for the operation, for example `rvz`, `iso`, `chd`, `nsz` |
| `{basename}` | The input filename without its extension |

Fallbacks: `{title}`, `{titleId}`, and `{serial}` fall back to the input basename when the metadata is missing; `{region}` and `{console}` resolve to an empty string. `{ext}` and `{basename}` are always available. Templating never requires keys: a Switch container read without `prod.keys`, or any file whose metadata cannot be decoded, still resolves through the basename fallbacks instead of failing the conversion.

Each resolved path component is sanitized for cross-platform safety: the characters `< > : " / \ | ? *` are replaced with `_`, control characters are stripped, trailing dots and spaces are trimmed, components are capped at 200 bytes on a UTF-8 boundary, and Windows reserved names (`CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9`) get a trailing `_`. A separator inside a token value (for example a `/` in a title) is replaced with `_` so a token cannot inject extra directories. The template may not escape the output root: a leading separator, a drive prefix, or any `..` component is rejected.

It composes with the other output controls. With `-R` the template is applied per file and the templated tree replaces the mirrored input subtree. `--on-conflict` is applied to the templated path, and the run report records the final templated path. `--output-template` conflicts with an explicit `OUTPUT` positional or `-o`/`--output`, since those name a single exact path. It is command-line only and is not read from the config file. `wup compress` does not accept it, because it packs many inputs into a single `.wua`. For `chd extract`, which writes a `.bin`/`.cue` pair or an `.iso`, the template resolves the base path and the sidecars share its stem.

CTR (`ctr decrypt`/`compress`/`decompress`/`convert`) supports the template for single-file runs; its recursive runs use the existing mirrored layout.

The GUI exposes an Output template field on the compress, decompress, convert, and decrypt pages plus chd extract, single file only for CTR. It resolves the path through the same library functions, so the result matches the CLI. A live preview is a planned follow-up.

---

### Info

```
rom-converto <console> info <INPUT> [--json] [--save-icon DIR] [--keys FILE]
```

Inspect a ROM file or title directory and print the embedded metadata: title, version, region, content layout, age ratings, and the embedded icon (where the format carries one). Maker / company codes are resolved to the publisher name (table ported from Dolphin). Encrypted 3DS CIA inputs are decrypted on the fly so the NCCH header fields show real values, not garbage. No decryption files are written to disk. Add `--json` for a machine-readable payload (the Tauri GUI uses the same JSON shape).

| Subcommand | Coverage |
|---|---|
| `ctr info <FILE>` | CIA / NCSD / NCCH; SMDH multilingual titles, region lock, age ratings, 48x48 icon. Encrypted CIA is auto-decrypted to read the NCCH header. |
| `dol info <FILE>` | GameCube `.iso`, `.gcm`, or `.rvz`; boot.bin header, BNR1/BNR2 banner with 96x32 image, publisher name. |
| `rvl info <FILE>` | Wii `.iso`, `.rvz`, or `.wbfs`; disc header, partition layout, TMD (title id, IOS), IMET banner names, 192x64 banner image from `opening.bnr` (falls back to the icon). |
| `wup info <PATH>` | loadiine + NUS directories, `.wua` archives, and `.wud`/`.wux` disc images; TMD + meta.xml with multilingual names, region, age ratings, save sizes, GamePad requirement, supported accessories, mastering date, decoded `iconTex.tga` icon. Pass `--keys` with the disc master key file when reading a `.wud`/`.wux`. |
| `nx info <FILE>` | NSP / NSZ / XCI / XCZ; container listing, tickets, CNMT, NACP, JPEG icon. Reports compression status (NSP vs NSZ), distribution (digital vs cartridge), structure classifier (scene / converted / CDN / homebrew), base title id for patches and DLC, decoded language list, age ratings per rating board. Full info needs `--keys prod.keys`, partial info without. |
| `chd info <FILE>` | CHD v5; version, codecs, hunk geometry, SHA-1 triplet, per-track CHT2 metadata, VERS / DVD tags |
| `cso info <FILE>` | CSO / ZSO; format and version, block geometry, index shift, raw block count, compression ratio |

`--save-icon DIR` writes the embedded icon as `<title_id>.png` into `DIR`. Supported by `ctr`, `dol`, `rvl`, `nx`, and `wup` info; not supported by `chd` or `cso` (those formats carry no embedded artwork). `--keys` applies to `nx info` (path to `prod.keys`) and to `wup info` on `.wud`/`.wux` disc images (disc master key file). Other consoles ignore it and will return an error if it is passed.

Format notes:

- `.rvz` (Wii and GameCube) and `.wbfs` (Wii) are read directly. Only the disc areas that are actually needed get decompressed, so no temp files are written and memory stays at a few MB.
- `.wua` (Wii U Cemu archive) is read directly. When an archive bundles base + update + DLC, the base title is shown, the bundled titles are listed, and the version includes the update.
- `.cso` / `.zso` are read directly.
- WIA, GCZ, NFS, and TGC are not supported.

---

### Shell completions

```
rom-converto shell-completions <SHELL> [--out-dir DIR]
```

Generates a tab-completion script for the rom-converto CLI. Writes to stdout by default. Pass `--out-dir DIR` to write the canonical per-shell filename inside `DIR` and print the resulting path.

| Shell | Install one-liner |
|---|---|
| Bash | `rom-converto shell-completions bash > ~/.local/share/bash-completion/completions/rom-converto` |
| Zsh | `rom-converto shell-completions zsh > "${fpath[1]}/_rom-converto"` |
| Fish | `rom-converto shell-completions fish > ~/.config/fish/completions/rom-converto.fish` |
| PowerShell | `rom-converto shell-completions powershell >> $PROFILE` |
| Elvish | `rom-converto shell-completions elvish > ~/.elvish/lib/rom-converto.elv` |

---

### Self-Update

```
rom-converto self-update
```

Checks GitHub for a newer release and replaces the current binary in place.

## Run Reports

Pass `--report <FILE>` to `compress`, `decompress`, `chd extract`, or `hash` to write a structured report of the run. The format is chosen from the file extension: `.csv` writes CSV, `.json` writes JSON, `.html` and `.htm` write a self-contained HTML table. Any other extension, or no extension, writes JSON. The report file is always created and overwritten directly; it does not go through `--on-conflict`. The numbers match the closing summary line.

The CTR (3DS) commands and all `verify` commands are not yet covered.

The `hash` command uses its own column schema, since it produces digests rather than a converted file: `path`, `crc32`, `sha1`, `md5`, `sha256`, `size_bytes`, `status`, `elapsed_ms`, `error`. Digest cells are empty for algorithms that were not requested, and there is no size-ratio column. The rest of this section describes the conversion report schema.

The columns are stable and appear in this order: `input_path`, `output_path`, `operation`, `status`, `input_bytes`, `output_bytes`, `ratio_pct`, `elapsed_ms`, `error`. `status` is `ok`, `skipped`, or `failed`. `input_bytes` and `output_bytes` are raw byte counts. `ratio_pct` is the space saved as a percent: positive when compression shrank the file, negative when decompression expanded it (honest expansion), and empty or `null` for skipped, failed, or zero-input rows. `output_path` and `error` are empty for skipped and failed rows.

The CSV file has a header row and one row per file, with RFC 4180 quoting for any field that contains a comma, double quote, or newline. It has no totals row.

The JSON file is an object with a `files` array and a `totals` object. Each entry in `files` has the fields above, with `ratio_pct` and `error` serialized as `null` when absent. The `totals` object has `total_files`, `ok`, `skipped`, `failed`, `total_input_bytes`, `total_output_bytes`, and `elapsed_ms`. An empty run writes `{"files":[],"totals":{...}}` with every total set to zero.

The HTML file is a single self-contained page with no external assets: one table with a header, one row per file (sizes shown human readable), and a totals row in the table footer.

## Benchmarks

RVZ, CHD, and Switch operations are measured against
`DolphinTool.exe` 2603a-x64, `chdman.exe` 0.284, and `nsz`
respectively, each tool on its own default settings, N = 10
interleaved warm runs. Full methodology and per-run detail live
alongside the results:

* [`benchmark/3DS.md`](benchmark/3DS.md): 3DS ROM results (Z3DS)
* [`benchmark/GameCube.md`](benchmark/GameCube.md): GameCube disc image results (RVZ)
* [`benchmark/Wii.md`](benchmark/Wii.md): Wii disc image results (RVZ)
* [`benchmark/Switch.md`](benchmark/Switch.md): Switch NSP/XCI results (NSZ/XCZ)
* [`benchmark/CHD.md`](benchmark/CHD.md): CD image results (CHD)

The Wii U `.wua` pipeline has no comparable reference CLI (Cemu ships the format but not a standalone compressor), so no head-to-head numbers are published for it.

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

Local development builds show the version as `dev-<shorthash>` (for example `dev-2dd4ee7`),
using the current git commit. The same applies to the GUI version label. If the source is not a
git checkout (for example a source tarball), the build falls back to the plain semantic version.

### Building Release Binaries

```
cargo build --release -p rom-converto-cli
```

The resulting binary will be at `target/release/rom-converto`.

Set `ROM_CONVERTO_RELEASE=1` at build time to mark a release build, which shows the normal
semantic version (for example `0.12.0`) instead of the dev hash. The release CI workflow sets
this automatically.

### Cross-Tool Parity Tests

Some tests cross-check output against the reference tools when they
are available. They are skipped unless the environment variable
points at the binary:

```
ROMCONVERTO_CHDMAN=$(which chdman) cargo test -p rom-converto-lib chdman
ROMCONVERTO_MAXCSO=$(which maxcso) cargo test -p rom-converto-lib maxcso
```

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

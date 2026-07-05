# CLI reference

This is the full reference for the `rom-converto` command line tool. It covers the global
flags, the behaviors shared across commands, and one section per command family. For a
quick overview see the [README](../README.md); for what each output format is see
[`formats.md`](formats.md).

Run `rom-converto --help` or `rom-converto <command> --help` for the same detail in the
terminal.

## Global flags

These flags work on every command.

| Flag | Description |
|---|---|
| `--dry-run` | Preview what the command would do without writing anything. See [Dry run](#dry-run) |
| `-v`, `--verbose` | Raise console verbosity. Repeatable up to `-vvv`. See [Verbosity](#verbosity) |
| `-q`, `--quiet` | Suppress everything except warnings and errors |
| `--debug-log <FILE>` | Write a full trace log to `FILE` regardless of console verbosity |
| `--config <FILE>` | Use this config file and skip the search order. See [`configuration.md`](configuration.md) |
| `--preset <NAME>` | Apply a named preset from the config |
| `--no-update-check` | Skip the background check for a newer release |
| `--skip-space-check` | Skip the free-space preflight before writing output. See [Disk-space preflight](#disk-space-preflight) |

## Shared behaviors

### Conflict policy

Every command that writes an output file except `migrate` takes `--on-conflict <POLICY>` to
decide what happens when the output already exists:

- `error` (default): refuse and stop.
- `overwrite`: replace the existing output.
- `skip`: leave the existing output and move on, counted as skipped in the summary.
- `rename`: write to the next free numbered sibling, so `Game.chd` becomes `Game (1).chd`.
- `overwrite-invalid`: verify the existing output, keep it if it passes (counted as
  skipped), and rewrite it if it fails or cannot be verified. A missing output is always
  written.

`-f`, `--force` is shorthand for `--on-conflict overwrite` and cannot be combined with
`--on-conflict`. For `wup decrypt` the output is a directory, so `rename` is not supported
there and falls back to `error`. For `chd extract` and `cue merge`, which write more than
one file, the policy applies to the base output path and the sidecars follow it.

`overwrite-invalid` runs the same integrity check the `verify` command does before
deciding. What it checks depends on the format:

| Command | What `overwrite-invalid` checks |
|---|---|
| `chd compress`, `cso compress` | Reads the produced file in full, about the cost of a decompression pass |
| `dol compress`, `rvl compress` | Verifies the `.rvz` against its stored SHA-1 structural hashes, fast, no group data decompressed |
| `nx compress` | Decrypts every NCA section and checks the hash hierarchy; needs `prod.keys`. Falls back to existence-based skip when keys are absent |
| `chd extract`, `cso decompress`, `dol`/`rvl`/`nx decompress` | Raw output has no integrity check, so it falls back to existence-based skip |

Recursive `ctr` commands manage their own output paths and behave the same way under this
policy.

### Dry run

`--dry-run` previews a command without writing any output. It prints one plan line per
file showing the operation, the resolved output path, the conflict decision, the detected
media or format, and any missing keys, for example
`Would compress game.iso -> game.cso (CSO) [overwrite]`. Under `overwrite-invalid` the
verify is read-only, so the preview runs it and shows `[keep (valid)]` or
`[rewrite (invalid)]` for an existing output. It runs the same input resolution,
detection, and conflict checks as a real run, exits 0 on a valid plan, and exits nonzero
only for real input errors such as a missing file. Pass `--report` alongside it to export
the plan.

For the recursive `ctr` file batches (`decrypt`, `compress`, `decompress`, `verify`) the
preview lists resolved output paths only, since those batches do not expose a per-file
conflict policy. Recursive `cdn-to-cia` does honor `--on-conflict`, so its preview shows
the decision per produced `.cia`.

### Verbosity

Console verbosity is a three-step ladder. `-v` shows debug-level messages from the
rom-converto modules, `-vv` raises them to trace level, and `-vvv` shows trace-level
output from every module including dependencies. `--quiet` suppresses everything except
warnings and errors and takes precedence over `-v`.

Separately, `--debug-log <FILE>` writes a full trace log (every module at trace level,
with timestamps and module targets) to `FILE` for the current run, regardless of console
verbosity. The file is created fresh at startup and is useful for attaching a complete log
to a bug report without flooding the terminal. If it cannot be opened, the command stops
with an error before doing any work.

### Disk-space preflight

Before any write-producing operation, the CLI estimates how much space the outputs need,
using the total size of the input files as a conservative floor, and checks the free space
on the output filesystem. If there is not enough room it aborts before writing anything,
naming the directory, the estimated need, and the space available. This is a best-effort
check: it cannot know exact output sizes, and decompression in particular can produce far
more than the compressed input, so the estimate is a floor. The value is catching a
near-full disk before a long batch starts. If the free-space query fails, the check is
skipped and the run proceeds. Under `--dry-run` nothing is written, so the check never
aborts. Pass `--skip-space-check` to disable the preflight.

### Run reports

Pass `--report <FILE>` to `compress`, `decompress`, `chd extract`, `hash`, or the
`dat verify`, `scan`, and `rename` commands to write a structured report after the run.
The format is chosen from the file extension: `.csv` writes CSV, `.json` writes JSON,
`.html` and `.htm` write a self-contained HTML table, and any other extension writes JSON.
The report file is overwritten directly and does not go through `--on-conflict`. The
numbers match the closing summary line.

The conversion report columns are stable and in this order: `input_path`, `output_path`,
`operation`, `status`, `input_bytes`, `output_bytes`, `ratio_pct`, `elapsed_ms`, `error`.
`status` is `ok`, `skipped`, or `failed`. `ratio_pct` is the space saved as a percent,
positive when compression shrank the file and negative when decompression expanded it, and
empty or `null` for skipped, failed, or zero-input rows. Extract rows carry zero byte
sizes since extraction writes several files.

The JSON file is an object with a `files` array and a `totals` object
(`total_files`, `ok`, `skipped`, `failed`, `total_input_bytes`, `total_output_bytes`,
`elapsed_ms`). The CSV file has a header row and one row per file with RFC 4180 quoting and
no totals row. The HTML file is a single self-contained page with a totals row in the table
footer. The `hash` command uses its own column schema, since it produces digests rather
than a converted file: `path`, `crc32`, `sha1`, `md5`, `sha256`, `size_bytes`, `status`,
`elapsed_ms`, `error`.

The `dat verify`, `scan`, and `rename` commands use their own schema too, since they record
database verdicts rather than a converted file: `path`, `verdict`, `game_name`, `game_id`,
`platform`, `signature_group`, `dat_version`, `match_algo`, `detail`, `size_bytes`,
`status`, `elapsed_ms`, `error`. `status` is `ok` or `failed`, separate from the finer
`verdict` (`verified`, `matched`, `hint`, `unknown`, `misnamed`, `renamed`, `skipped`,
`unsupported`, or `failed`); an `unsupported` verdict still counts as `ok`. The JSON file wraps the records
in a `files` array with a `totals` object (`total_files`, `ok`, `skipped`, `failed`,
`total_input_bytes`, `total_output_bytes`, `elapsed_ms`).

### Output-path templates

`--output-template <STRING>` builds each output path from tokens filled by the metadata
rom-converto already reads (the same data the `info` command shows). It turns a flat folder
of ROMs into a sorted tree in one recursive run and uses only in-tool metadata, never an
external DAT.

```
rom-converto dol compress -R roms/ --output-dir organized/ --output-template "{console}/{title}.{ext}"
```

The template is a relative path. Tokens are written as `{name}` and any other text is kept
literally, so `/` in the template creates a subdirectory. The resolved path is joined under
`--output-dir`, or the current directory when no output directory is given.

| Token | Resolves to |
|---|---|
| `{title}` | 3DS SMDH short title, GameCube banner or header name, Wii IMET or header name, Wii U meta.xml long name, Switch NACP title. Prefers the English entry |
| `{titleId}` | 3DS title id, GameCube/Wii game id, Wii TMD title id (hex), Wii U title id (hex), Switch application id (hex) |
| `{region}` | 3DS SMDH region, GameCube/Wii region, Wii U region list. Empty for Switch and CHD/CSO |
| `{console}` | `3DS`, `GameCube`, `Wii`, `WiiU`, `Switch`, `CHD`, or `CSO` |
| `{serial}` | 3DS product code, GameCube/Wii game id, Wii U product code. Falls back to the basename otherwise |
| `{ext}` | The output extension for the operation, for example `rvz`, `iso`, `chd`, `nsz` |
| `{basename}` | The input filename without its extension |

`{title}`, `{titleId}`, and `{serial}` fall back to the input basename when the metadata is
missing; `{region}` and `{console}` resolve to an empty string. Each resolved path
component is sanitized for cross-platform safety: `< > : " / \ | ? *` become `_`, control
characters are stripped, trailing dots and spaces are trimmed, components are capped at 200
bytes on a UTF-8 boundary, and Windows reserved names get a trailing `_`. The template may
not escape the output root: a leading separator, a drive prefix, or any `..` component is
rejected.

`--output-template` conflicts with an explicit `OUTPUT` positional or `-o`/`--output`, and
is command-line only (not read from the config file). `wup compress` does not accept it,
because it packs many inputs into one `.wua`. CTR supports it for single-file runs; its
recursive runs use the mirrored layout.

### Cancellation

Pressing Ctrl-C stops the current operation cleanly. The running conversion aborts mid-file
at the next safe point, the partial output is removed, a `Cancelled` message is printed, and
the process exits with code 130. This covers every decrypt, encrypt, compress, and
decompress operation across all consoles. A pre-existing file chosen for overwrite is left
untouched, since the conversion writes to a temporary sibling and only renames it into place
once it finishes. In a batch run the file in progress is cancelled and the loop stops; files
already converted are kept. A cancelled run is reported with its own status rather than as a
failure.

### Progress

A recursive `-R` run shows two bars: an overall one pinned on top with files done/total,
total size processed, and an ETA for the whole batch, and the per-file bar below it with
that file's throughput and remaining time. Single-file runs only show the per-file bar.

### Summaries

After `compress`, `decompress`, and `convert` operations the tool prints a closing summary
of bytes processed and space saved or expanded, for example
`12 files: 12.4 GiB -> 4.1 GiB, saved 8.3 GiB (67%) in 2m14s`. Verify and extract operations
print a file count and elapsed time instead.

---

## ctr (Nintendo 3DS)

```
rom-converto ctr <SUBCOMMAND> <INPUT> [OUTPUT]
```

| Subcommand | Description |
|---|---|
| `cdn-to-cia <CDN_DIR> [OUTPUT]` | Convert a CDN directory to `.cia` |
| `generate-cdn-ticket <CDN_DIR> [OUTPUT]` | Generate a `.tik` ticket from CDN content |
| `decrypt <INPUT> [OUTPUT]` | Decrypt an encrypted ROM for emulator use |
| `compress <INPUT> [OUTPUT]` | Compress a decrypted ROM to Z3DS |
| `decompress <INPUT> [OUTPUT]` | Decompress a Z3DS file back to the original ROM |
| `convert <INPUT> [OUTPUT]` | Convert between `.cia` and `.cci`/`.3ds`, direction auto-detected |
| `verify <INPUT>` | Verify `.cia` legitimacy or `.3ds`/`.cci` NCCH integrity |
| `info <INPUT>` | Inspect 3DS metadata. See [info](#info) |

Format-specific flags (shared conflict, recursion, template, and report flags are covered
in [Shared behaviors](#shared-behaviors)):

| Flag | Applies to | Description |
|---|---|---|
| `--output-dir <DIR>` | `cdn-to-cia`, `decrypt`, `compress`, `decompress`, `convert` | Write outputs under this directory instead of beside each input |
| `-C, --cleanup` | `cdn-to-cia` | Remove original CDN files after conversion |
| `-T, --ensure-ticket-exists` | `cdn-to-cia` | Generate a ticket file if one is not found |
| `-D, --decrypt` | `cdn-to-cia` | Also decrypt the CIA after creation |
| `-Z, --compress` | `cdn-to-cia` | Also compress the CIA after creation (implies decrypt) |
| `-l, --level <LEVEL>` | `compress` | Zstd compression level 0..=22 (0 = library default, 22 = maximum ratio) |
| `--allow-encrypted` | `compress` | Compress even if the input ROM appears encrypted. By default an encrypted ROM is refused; decrypt first with `ctr decrypt` |
| `--full` | `verify` | Also verify content hashes against the TMD (CIA only, slower). `--verify-content` is an alias |

Generated tickets from `generate-cdn-ticket` use placeholder values and only work on modded
consoles and emulators. `decrypt` supports `.cia`, `.3ds`, `.cci`, and `.cxi`, with the
format detected automatically; place a `seeddb.bin` next to the executable to resolve seeds
locally, otherwise the seed is fetched from Nintendo's API. `compress` inspects the crypto
flags and refuses an input that still looks encrypted, pointing you to `ctr decrypt`, unless
you pass `--allow-encrypted`. `convert` produces an unsigned CIA with a zero title key,
compatible with CFW and emulators but not installable on stock hardware.

## dol (GameCube)

```
rom-converto dol <SUBCOMMAND> <INPUT> [OUTPUT]
```

| Subcommand | Description |
|---|---|
| `compress <INPUT> [OUTPUT]` | Compress a `.iso`/`.gcm` to Dolphin's `.rvz` |
| `migrate <INPUT> [OUTPUT]` | Migrate a legacy `.gcz`, `.nkit.iso`, or `.nkit.gcz` to `.rvz` with an integrity check first |
| `decompress <INPUT> [OUTPUT]` | Decompress a `.rvz` back to `.iso` |
| `verify <INPUT>` | Verify a `.iso`, `.gcm`, `.rvz`, or legacy `.gcz`/NKit image (checks RVZ container hashes, or a whole-disc SHA-1 with `--full`) |
| `info <INPUT>` | Inspect GameCube disc metadata. See [info](#info) |

| Flag | Applies to | Description |
|---|---|---|
| `-l, --level <LEVEL>` | `compress`, `migrate` | Zstandard compression level (defaults to 22, Dolphin's max non-extreme) |
| `--chunk-size <BYTES>` | `compress`, `migrate` | Chunk size in bytes, power of two between 32 KiB and 2 MiB (defaults to 128 KiB to match Dolphin) |
| `--output-dir <DIR>` | `compress`, `decompress` | Write outputs under this directory instead of beside each input |
| `--skip-verify` | `migrate` | Skip the pre-conversion integrity pass |
| `--full` | `verify` | Decode the whole disc and compute a whole-disc SHA-1 |

Output is byte-identical to Dolphin's own encoder and decoder in both directions.

`migrate` integrity-checks the source first (GCZ block checksums, NKit whole-file CRC32),
regenerates NKit junk data, and streams the rebuilt disc straight to `.rvz` with no
temporary files. The input format is detected by content, so renamed files still work.
Unlike the other commands, `migrate` overwrites an existing output only with `-f`/`--force`
and does not take `--on-conflict`. Without `--force`, a single-file run stops on an existing
output, while a recursive run skips it and continues.

`dol verify` reads the same legacy GameCube containers as `migrate` (`.gcz`, NKit); a `.wia` holds a Wii disc image and is rejected with a pointer to `rvl verify`.

Advisory warning: `--chunk-size` above 1 MiB on `compress` or `migrate` prints a warning
that large chunks can stutter on weaker playback hardware, and suggests re-encoding at
128 KiB. `rvl compress` and `rvl migrate` share the same RVZ pipeline and the same warning.

## rvl (Wii)

```
rom-converto rvl <SUBCOMMAND> <INPUT> [OUTPUT]
```

| Subcommand | Description |
|---|---|
| `compress <INPUT> [OUTPUT]` | Compress a `.iso`/`.wbfs` to Dolphin's `.rvz` |
| `migrate <INPUT> [OUTPUT]` | Migrate a legacy `.wia`, `.gcz`, `.nkit.iso`, or `.nkit.gcz` to `.rvz` with an integrity check first |
| `decompress <INPUT> [OUTPUT]` | Decompress a `.rvz` back to `.iso` |
| `verify <INPUT>` | Verify a `.iso`, `.wbfs`, `.rvz`, or legacy `.wia`/`.gcz`/NKit image (checks RVZ container hashes, or recomputes the Wii partition hash tree with `--full`) |
| `info <INPUT>` | Inspect Wii disc metadata. See [info](#info) |

`rvl migrate` covers `.wia` in every codec (bzip2, LZMA, LZMA2, purge, none) alongside
`.gcz` and NKit. It rebuilds the Wii hash tree and re-encrypts partitions on the fly while
converting to `.rvz`.

Flags match the `dol` commands, including `--output-dir` on `compress` and `decompress`
and the shared `migrate` flags. `rvl migrate` additionally takes `--deep`, which decodes
every WIA group during verification instead of only the SHA-1 header chain (GCZ and NKit
checks are already exhaustive, so it applies to WIA input only).
`--full` on `rvl verify` decrypts every partition cluster and recomputes the H0/H1/H2 hash
tree. `dol` and `rvl` share one RVZ pipeline, and output is byte-identical to Dolphin on
both consoles.

## wup (Wii U)

```
rom-converto wup <SUBCOMMAND> ...
```

| Subcommand | Description |
|---|---|
| `compress -o <OUTPUT> <INPUTS>...` | Bundle one or more titles into a Cemu `.wua` archive |
| `decrypt -o <OUTPUT> <INPUT>` | Decrypt a NUS directory into a loadiine `meta/code/content` tree |
| `verify <INPUT>` | Verify Wii U content SHA-1 against the TMD |
| `info <PATH>` | Inspect Wii U title metadata. See [info](#info) |

| Flag | Applies to | Description |
|---|---|---|
| `-o, --output <FILE>` | `compress` | Output `.wua` file path |
| `-o, --output <DIR>` | `decrypt` | Output directory |
| `-l, --level <LEVEL>` | `compress` | Zstd compression level 0..=22 (0 = Cemu default of 6) |
| `--key <KEYFILE>` | `compress`, `verify` | Disc master key file for `.wud`/`.wux` inputs. Pass once per disc input in positional order on `compress` |

`wup` commands do not take `--output-dir` or `--output-template`, because `compress` packs
many inputs into a single archive and `decrypt` writes a directory tree. `compress`
auto-detects each input as a loadiine directory, a NUS directory, or a disc image, and
resolves disc keys from `--key`, a sibling `<disc>.key` file, or `game.key`. `decrypt`
handles both the canonical Nintendo layout (`title.tmd` + `title.tik` + `{id}.app`) and the
community layout variant (`tmd.<N>` + optional `cetk.<N>` + extensionless content files);
when no ticket is present the title key is derived from the title id.

## nx (Nintendo Switch)

```
rom-converto nx <SUBCOMMAND> <INPUT> [-o OUTPUT]
```

| Subcommand | Description |
|---|---|
| `compress <INPUT> [-o OUTPUT]` | Compress a `.nsp` to `.nsz` or a `.xci` to `.xcz` |
| `decompress <INPUT> [-o OUTPUT]` | Decompress a `.nsz`/`.xcz` back to `.nsp`/`.xci` |
| `verify <INPUT>` | Verify per-NCA hash integrity of any Switch container |
| `info <INPUT>` | Inspect Switch container metadata. See [info](#info) |

| Flag | Applies to | Description |
|---|---|---|
| `--keys <PRODKEYS>` | all | Path to `prod.keys`. Defaults to `$HOME/.switch/prod.keys` (`%USERPROFILE%/.switch/prod.keys` on Windows) |
| `-l, --level <LEVEL>` | `compress` | Zstd compression level 1..=22 (defaults to 18, matching `nsz`) |
| `--mode <MODE>` | `compress` | `solid` (one zstd frame per NCA, default for NSP) or `block` (default for XCI) |
| `--block-size-exp <EXP>` | `compress` | Block-mode block size as `1 << exp` bytes, range 14..=32 (defaults to 20 = 1 MiB) |
| `--output-dir <DIR>` | `compress`, `decompress` | Write outputs under this directory instead of beside each input |

`prod.keys` is required to derive the per-NCA section keys; the file is read but never
modified. Output is byte-identical to `nsz` and `nsz -D` at matching settings, and `verify`
works on already-compressed containers without decompressing first.

## chd (CD / DVD)

```
rom-converto chd <SUBCOMMAND> <INPUT> [OUTPUT]
```

| Subcommand | Description |
|---|---|
| `compress <INPUT> [OUTPUT]` | Compress a `.cue` or `.iso` to `.chd`; CD vs DVD media is auto-detected |
| `extract <INPUT> [OUTPUT]` | Extract a `.chd` back to `.bin` + `.cue` (CD) or `.iso` (DVD) |
| `verify <INPUT>` | Verify the SHA-1 integrity of a `.chd` |
| `to-cso <INPUT> [OUTPUT]` | Extract a DVD-mode `.chd` straight to `.cso` (default) or `.zso`, through a temporary ISO |
| `info <INPUT>` | Inspect CHD metadata. See [info](#info) |

| Flag | Applies to | Description |
|---|---|---|
| `--dvd` / `--cd` | `compress` | Override the auto-detected mode (CD mode needs a cue sheet) |
| `--hunk-size <BYTES>` | `compress` | DVD hunk size, a multiple of 2048; defaults to 4096, or 2048 for detected PSP images |
| `--zstd` | `compress` | Add zstd to the DVD codec set for a better ratio; some older players and cores do not support zstd-compressed CHD |
| `--format <cso\|zso>` | `to-cso` | Output container: CSO for PSP/PPSSPP, ZSO for PS2 via Open PS2 Loader |
| `--block-size <BYTES>` | `to-cso` | Block size, a power of two; defaults to 2048 (16384 for 2 GiB+ inputs) |
| `--output-dir <DIR>` | `compress`, `extract`, `to-cso` | Write outputs under this directory instead of beside each input |
| `-p, --parent <PARENT>` | `extract`, `verify` | Specify a parent CHD for parent-child relationships |
| `--fix` | `verify` | Correct SHA-1 values in the CHD header if mismatches are found |

`compress` probes the CD/DVD media type from the image, so the createcd versus createdvd
mixup cannot happen. Extract report rows carry zero byte sizes since extraction writes
several files.

`to-cso` only accepts a DVD-mode CHD (PS2 DVD, PSP UMD); a CD-mode CHD has no flat ISO for
CSO/ZSO to hold, and is rejected up front. It extracts to a temporary ISO next to the output,
runs the same CSO/ZSO writer `cso compress` uses, and always removes the temporary ISO
afterward, whether the run succeeds, fails, or is cancelled.

Advisory warning: compressing a `.cue` whose data track carries the Dreamcast IP.BIN
signature into a CD-mode CHD prints a warning, since some cores only boot Dreamcast from a
GDI-based image. Convert from the GDI-based image instead if the CHD does not boot.

## cso (PSP / PS2)

```
rom-converto cso <SUBCOMMAND> <INPUT> [OUTPUT]
```

| Subcommand | Description |
|---|---|
| `compress <INPUT> [OUTPUT]` | Compress an `.iso` to `.cso` (default) or `.zso` |
| `decompress <INPUT> [OUTPUT]` | Restore the original `.iso` from a `.cso`/`.zso`/`.dax` |
| `verify <INPUT>` | Validate the container structure; `--full` decodes every block |
| `to-chd <INPUT> [OUTPUT]` | Compress a `.cso`/`.zso`/`.dax` straight to `.chd`, through a temporary ISO |
| `info <INPUT>` | Inspect CSO/ZSO/DAX metadata. See [info](#info) |

| Flag | Applies to | Description |
|---|---|---|
| `--format <cso\|zso>` | `compress` | Output container: CSO for PSP/PPSSPP, ZSO for PS2 via Open PS2 Loader |
| `--block-size <BYTES>` | `compress` | Block size, a power of two; defaults to 2048 (16384 for 2 GiB+ inputs) |
| `--dvd` / `--cd` | `to-chd` | Override the auto-detected mode of the decoded ISO (CD mode needs a cue sheet) |
| `--hunk-size <BYTES>` | `to-chd` | DVD hunk size, a multiple of 2048; defaults to 4096, or 2048 for detected PSP images |
| `--zstd` | `to-chd` | Add zstd to the DVD codec set for a better ratio; some older players and cores do not support zstd-compressed CHD |
| `--output-dir <DIR>` | `compress`, `decompress`, `to-chd` | Write outputs under this directory instead of beside each input |
| `--full` | `verify` | Decode every block instead of only checking the index |

Defaults are maxcso-compatible: 2 KiB blocks (16 KiB for 2 GiB+ inputs), automatic index
shift for large images, and a per-block store-raw fallback.

`to-chd` decodes to a temporary ISO next to the output, then runs the same disc-to-CHD
writer `chd compress` uses (so any embedded GAME/NAME tags match a direct build), and always
removes the temporary ISO afterward, whether the run succeeds, fails, or is cancelled.

`decompress`, `verify`, `to-chd`, and `info` also accept legacy `.dax` (PSP) input; the
container is detected by its magic, not its extension. DAX is decode-only, so `compress`
still writes CSO or ZSO only.

## cue (CD)

```
rom-converto cue merge <INPUT_CUE> <OUTPUT_CUE>
```

Merge a multi-bin `.cue` (one `.bin` per track) into a single `.bin` + `.cue` pair, for
emulators that cannot load split images. The merged `.bin` is named after the output `.cue`.
`merge` takes `--on-conflict` (and `-f`) only; the `.bin` sidecar follows the renamed `.cue`.

## dat

```
rom-converto dat <SUBCOMMAND> ...
```

| Subcommand | Description |
|---|---|
| `verify <INPUT>` | Verify a ROM's decoded content hashes against the Playmatch database |
| `scan <DIR>` | Batch-identify a library and summarize matched, misnamed, and unknown files |
| `rename <INPUT>` | Rename ROMs to their canonical database names |
| `identify <INPUT>` | Look up one file and print everything the database knows about it |
| `fixdat <DIR> -o <FILE>` | Build a Logiqx fixdat of the database entries missing from a local library |

Format-specific flags (shared conflict and report flags are covered in
[Shared behaviors](#shared-behaviors)):

| Flag | Applies to | Description |
|---|---|---|
| `--algo <ALGOS>` | `verify`, `identify` | Comma-separated digests: `crc32`, `sha1`, `md5`, `sha256`. Default `crc32,sha1` |
| `-R`, `--recursive` | `verify`, `rename` | Process every file under INPUT, descending into subdirectories |
| `--max-depth <N>` | `verify`, `scan`, `rename`, `fixdat` | Limit recursion depth. `1` = top level only. On `verify` and `rename` requires `-R`; `scan` and `fixdat` always walk the whole directory |
| `--report <FILE>` | `verify`, `scan`, `rename` | Write a run report. See [Run reports](#run-reports) |
| `--api-base <URL>` | all | Playmatch API base URL for this run. Defaults to the public instance |
| `-o, --output <FILE>` | `fixdat` | Path for the generated Logiqx fixdat. Required |
| `--platform <NAME>` | `fixdat` | Select the source DAT by platform name. Required unless `--dat-id` is given |
| `--dat-id <UUID>` | `fixdat` | Select the source DAT by exact id, skipping the platform lookup |
| `--dat-name <NAME>` | `fixdat` | Narrow the candidate DATs by name substring. Requires `--platform` |
| `--subset <SUBSET>` | `fixdat` | Narrow the candidate DATs by subset. Requires `--platform` |

Every file is hashed on its decoded inner stream, not the compressed container bytes, so a
`.chd`, `.rvz`, `.wbfs`, `.cso`, `.zso`, `.gcz`, `.wia`, NKit, or Z3DS file verifies the same
as the raw ROM or disc image it holds. GCZ, WIA, and NKit containers are detected by content,
so a renamed file still verifies correctly. Multi-track discs check every track. `.nsz` and
`.xcz` have no inner hasher and are reported as `unsupported` while the run continues. A
`.cue` file is never hashed on its own: a recursive walk groups each cue with the `.bin`
tracks it lists and hashes those, and `rename` always leaves a cue set untouched so its
`FILE` lines stay consistent.

Advisory warning: `verify` and `scan` print a warning once per run when any file reports
`unsupported`, explaining that compressed Switch containers (`.nsz`, `.xcz`) need
`nx decompress` first, which needs a `prod.keys` file.

`verify` treats a filename-and-size match as a hint and reports it as not verified, while
`identify` shows the same match as a weak result so a near-miss is still informative.
`rename` renames only on a hash-verified match; a hint never renames a file. The target
name is the game's canonical file name when the match resolves to exactly one database
file entry with the same extension as the local file, and the game name otherwise. `scan`
is always recursive over its directory, and it and `rename` hash with `crc32` and `sha1`,
while `fixdat` indexes the local library with all four digests. `--algo` widens the digest
set on `verify` and `identify` only.

`fixdat` needs either `--platform` or `--dat-id` to pick a source DAT; `--dat-name` and
`--subset` narrow an ambiguous platform match, and more than one remaining candidate stops
the run with each candidate listed. `--api-base` points every subcommand at a different
Playmatch instance and defaults to the public one at
`https://playmatch.retrorealm.dev/api/v2`.

## hash

```
rom-converto hash <INPUT> [--algo crc32,sha1,md5,sha256] [-R] [--max-depth N] [--report FILE]
```

Compute plain checksums for a file, or for every file in a directory with `-R`. This is a
digest tool only: it reads the bytes and prints the hashes, with no DAT or database lookup.

| Flag | Description |
|---|---|
| `<INPUT>` | A file, or a directory with `-R` |
| `--algo <ALGOS>` | Comma-separated digests: `crc32`, `sha1`, `md5`, `sha256`. Default `crc32,sha1` |
| `-R`, `--recursive` | Hash every file under INPUT, descending into subdirectories |
| `--max-depth <N>` | Limit recursion depth when `-R` is set. `1` = top level only. Requires `-R` |
| `--report <FILE>` | Write a run report. See [Run reports](#run-reports) |

All digests are computed in a single streaming pass per file, so memory stays constant no
matter how large the input is. Hashes print as lowercase hex. Unreadable files are reported
and skipped without aborting the batch.

## playlist

```
rom-converto playlist <DIR> [--playlist-mode multiple|always] [--ext EXTS] [--output-dir DIR] [--max-depth N] [--on-conflict POLICY] [-f]
```

Scan a directory for disc images and write one `.m3u` per multi-disc game so emulators can
swap discs. Grouping is filename-based only; no DAT lookup is done. The scan is recursive by
default.

| Flag | Description |
|---|---|
| `<DIR>` | Directory to scan for disc images |
| `--playlist-mode <MODE>` | `multiple` (default) writes an `.m3u` only for sets with more than one disc; `always` also writes a single-entry `.m3u` for single-disc games |
| `--ext <EXTS>` | Comma-separated disc image extensions to scan. Default `cue,chd,iso,cso,zso` |
| `--output-dir <DIR>` | Write the `.m3u` files here instead of beside the disc files |
| `--max-depth <N>` | Limit scan depth. `1` = top level only. Omit for unlimited |
| `--on-conflict <POLICY>`, `-f` | What to do when an `.m3u` already exists. `overwrite-invalid` has no integrity check for plain text, so it behaves as `skip` |

The grouping matches the Redump and TOSEC disc-token conventions. The base title is the
filename with the disc token and its surrounding parentheses removed; the `.m3u` is named
after that base title. The match is case-insensitive on the word "Disc" and requires a
preceding `(` or whitespace, so titles that genuinely contain it are never mis-grouped. Disc
numbers sort numerically, mixed extensions in one set are grouped together, and entries are
relative paths with forward slashes.

Advisory warning: a set that mixes more than one track format (for example `.cue` and
`.chd` in the same game) prints a warning, since emulators expect every disc in a playlist
to use the same format.

## info

```
rom-converto <console> info <INPUT> [--json] [--save-icon DIR] [--keys FILE]
```

Inspect a ROM file or title directory and print the embedded metadata: title, version,
region, content layout, age ratings, and the embedded icon where the format carries one.
Maker and company codes are resolved to the publisher name. Encrypted 3DS CIA inputs are
decrypted on the fly to read the NCCH header, and nothing is written to disk. Add `--json`
for a machine-readable payload (the GUI uses the same shape).

For `dol` and `rvl`, the report names the container it read: the text output prints it as
`Format: GameCube (GCZ)` or `Format: Wii (WIA)`, and `--json` carries it as the `container`
field (`ISO`, `RVZ`, `WBFS`, `GCZ`, `WIA`, or `NKit`).

| Flag | Description |
|---|---|
| `--json` | Emit a machine-readable payload instead of the formatted report |
| `--save-icon <DIR>` | Write the embedded icon as `<title_id>.png` into `DIR`. Supported by `ctr`, `dol`, `rvl`, `nx`, and `wup`; `chd` and `cso` carry no artwork |
| `--keys <FILE>` | `prod.keys` for `nx info`, or a disc master key file for `wup info` on `.wud`/`.wux`. Other consoles do not use it |

Coverage per family: `ctr` reads CIA/NCSD/NCCH and Z3DS variants; `dol` reads `.iso`,
`.gcm`, `.rvz`, `.gcz`, and NKit; `rvl` reads `.iso`, `.rvz`, `.wbfs`, `.wia`, `.gcz`, and
NKit through the same streaming migration readers the `migrate` command uses; `wup` reads
loadiine and NUS directories, `.wua` archives, and `.wud`/`.wux` disc images; `nx` reads
NSP/NSZ/XCI/XCZ; `chd` reads CHD v5; `cso` reads CSO/ZSO/DAX. NFS and TGC are not
supported.

## shell-completions

```
rom-converto shell-completions <SHELL> [--out-dir DIR]
```

Generate a tab-completion script. Writes to stdout by default. Pass `--out-dir DIR` to write
the canonical per-shell filename inside `DIR` and print the resulting path. Supported shells
are bash, zsh, fish, powershell, and elvish, for example:

```
rom-converto shell-completions bash > ~/.local/share/bash-completion/completions/rom-converto
rom-converto shell-completions zsh > "${fpath[1]}/_rom-converto"
```

## self-update

```
rom-converto self-update
```

Check GitHub for a newer release and replace the current binary in place.

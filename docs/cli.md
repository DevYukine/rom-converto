# CLI reference

Complete reference for the `rom-converto` command line tool. See the [README](../README.md)
for installation and quick-start examples, or [Formats](formats.md) for format details.

Use `rom-converto --help`, `rom-converto <command> --help`, or
`rom-converto <command> <subcommand> --help` for built-in help.

## Commands

| Command | Purpose |
|---|---|
| `nds` | Encrypt, decrypt, and inspect Nintendo DS ROMs |
| `ctr` | Convert, decrypt, compress, and verify Nintendo 3DS ROMs |
| `dol` | Compress, migrate, and verify GameCube disc images (RVZ) |
| `rvl` | Compress, migrate, and verify Wii disc images (RVZ) |
| `wup` | Bundle and decrypt Wii U titles (WUA) |
| `nx` | Compress, decompress, verify, and inspect Switch containers; merge or split unpacked NSP/XCI |
| `chd` | Compress, extract, and verify CD/DVD/LaserDisc images (CHD) |
| `cso` | Compress and verify PSP/PS2 ISOs (CSO/ZSO) |
| `cue` | Merge a multi-bin `.cue` into one `.bin`/`.cue` pair |
| `xbox` | Convert, extract, and inspect Original Xbox disc images (XISO) |
| `xenon` | Compress Xbox 360 images to ZAR, convert ISO to GoD, extract, verify, and inspect |
| `ps3` | Decrypt and inspect PlayStation 3 disc images |
| `psp` | Inspect and extract PSP `EBOOT.PBP` containers |
| `vita` | Inspect PS Vita VPK/PKG packages and extract a PKG |
| `info` | Auto-detect the console and inspect any supported ROM or disc image |
| `capabilities` | Print the supported operations and info extensions as JSON |
| `dat` | Identify, verify, and rename ROMs against the Playmatch database |
| `hash` | Compute CRC32, SHA-1, MD5, and SHA-256 digests |
| `playlist` | Generate `.m3u` files for multi-disc sets |
| `shell-completions` | Print a tab-completion script for your shell |
| `self-update` | Replace the binary with a newer GitHub release |
| `help [COMMAND]` | Show help for the CLI or a command |

## Global flags

These flags work before or after any subcommand.

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
| `--no-cache` | Ignore the persistent hash and verify cache for this run. See [Hash and verify cache](#hash-and-verify-cache) |
| `--rebuild-cache` | Discard the cache and rebuild it from this run |
| `-h`, `--help` | Show help |

`-V` and `--version` show the version at the top level. Use
`rom-converto help <command>` or `rom-converto <command> --help` for command help.

## Shared behaviors

### Common output and batch flags

These flags appear on most file conversion commands. Each command's `--help` shows the
exact set it accepts.

| Flag | Description |
|---|---|
| `[OUTPUT]` or `-o, --output <PATH>` | Set one output path. The positional and flag forms conflict |
| `--output-dir <DIR>` | Put derived outputs under `DIR`. Conflicts with an explicit output |
| `--output-template <TEMPLATE>` | Build output paths from metadata tokens. Conflicts with an explicit output |
| `-R, --recursive` | Process supported files under the input directory |
| `--max-depth <N>` | Limit recursive depth. `1` scans only the input directory. Requires `-R` unless the command is always recursive |
| `--on-conflict <POLICY>` | Choose how to handle an existing output |
| `-f, --force` | Overwrite an existing output. Conflicts with `--on-conflict` |
| `--report <FILE>` | Write a CSV, HTML, or JSON run report |

### Conflict policy

Most commands that write files take `--on-conflict <POLICY>`:

- `error` (default): refuse and stop.
- `overwrite`: replace the existing output.
- `skip`: keep the existing output and continue.
- `rename`: write to the next free numbered sibling, so `Game.chd` becomes `Game (1).chd`.
- `overwrite-invalid`: keep a valid output; rewrite an invalid or unverifiable output.

`-f`, `--force` means `--on-conflict overwrite`. The two flags conflict. `wup decrypt`
cannot rename an output directory, so `rename` acts as `error`. For `chd extract` and
`cue merge`, the policy applies to the main path and its sidecars.

`overwrite-invalid` runs the same integrity check the `verify` command does before
deciding. What it checks depends on the format:

| Command | What `overwrite-invalid` checks |
|---|---|
| `chd compress`, `cso compress` | Reads the produced file in full, about the cost of a decompression pass |
| `dol compress`, `rvl compress` | Verifies the `.rvz` against its stored SHA-1 structural hashes, fast, no group data decompressed |
| `nx compress` | Decrypts every NCA section and checks the hash hierarchy; needs `prod.keys`. Falls back to existence-based skip when keys are absent |
| `chd extract`, `cso decompress`, `dol`/`rvl`/`nx decompress` | Raw output has no integrity check, so it falls back to existence-based skip |

### Dry run

`--dry-run` resolves inputs and output paths, detects formats, checks conflicts, and prints
one plan line per file. It does not write converted ROMs or rename source files. It can
still write a requested `--report` or `--debug-log`, rebuild the persistent cache with
`--rebuild-cache`, and contact services used for metadata, DAT lookup, seeds, or update
checks. Add `--no-update-check` when a fully local preview is required.

With `overwrite-invalid`, the preview reads and verifies an existing output before showing
`keep (valid)` or `rewrite (invalid)`. Recursive `ctr` batches show resolved output paths;
recursive `ctr cdn-to-cia` also shows each conflict decision.

### Verbosity

`-v` shows debug messages, `-vv` shows rom-converto trace messages, and `-vvv` includes
dependency traces. `--quiet` wins over `-v`. `--debug-log <FILE>` always writes a fresh,
timestamped trace log. A log-open failure stops the command.

### Disk-space preflight

Before writing output, the CLI checks that the output filesystem has at least as much free
space as the input size. This is a floor, especially for decompression. A failed free-space
query does not stop the run. `--dry-run` skips this check. Use `--skip-space-check` to
disable it.

### Hash and verify cache

`hash -R`, DAT commands, and `overwrite-invalid` checks cache digests and verification
results in `<config dir>/rom-converto/hash-cache.json.gz`. Entries are invalidated when file
size or modification time changes. DAT commands still query the remote database.
`--no-cache` ignores the cache. `--rebuild-cache` deletes it first and repopulates it.

### Archive input

`nx merge`, `nx split`, and `xenon convert` require unpacked inputs in the CLI.

Single-file operations can read `.zip`, `.7z`, `.rar`, `.tar`, `.tar.gz`, and `.tgz`
archives. This applies to conversion, extraction, verification, and info under `ctr`, `dol`,
`rvl`, `nx`, `chd`, `cso`, and `ps3`, including `chd to-cso` and `cso to-chd`. It also
applies to `hash`, `dat verify`, and `dat identify`.

The first matching member in name order is extracted to a temporary directory. Cue track
files are extracted with their cue sheet. Output is named after the member and written next
to the archive unless another output path is set. Recursive walkers do not open archives.
Encrypted archives and plain `.gz` files are unsupported. RAR input is read only. The temp
filesystem needs room for the unpacked input.

### Run reports

`--report <FILE>` is available on conversion commands, `chd extract`, `ps3 decrypt`,
`nds encrypt` and `decrypt`, `hash`, and `dat verify`, `scan`, and `rename`. `.csv` writes
CSV, `.html` or `.htm` writes HTML, and all other extensions write JSON. Reports overwrite
the target directly and ignore `--on-conflict`.

The conversion report columns are stable and in this order: `input_path`, `output_path`,
`operation`, `status`, `input_bytes`, `output_bytes`, `ratio_pct`, `elapsed_ms`, `error`.
`status` is `ok`, `skipped`, or `failed`. `ratio_pct` is the space saved as a percent,
positive when compression shrank the file and negative when decompression expanded it, and
empty or `null` for skipped, failed, or zero-input rows. Extract rows carry zero byte
sizes since extraction writes several files.

JSON contains `files` and `totals`. CSV has one header and one row per file. HTML is
self-contained. Hash reports use `path`, digest columns, `size_bytes`, `status`,
`elapsed_ms`, and `error`.

DAT reports use: `path`, `verdict`, `game_name`, `game_id`,
`platform`, `signature_group`, `dat_version`, `match_algo`, `detail`, `size_bytes`,
`status`, `elapsed_ms`, `error`. `status` is `ok` or `failed`, separate from the finer
`verdict` (`verified`, `matched`, `hint`, `unknown`, `misnamed`, `renamed`, `skipped`,
`unsupported`, or `failed`). `unsupported` still counts as `ok`.

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
recursive runs use the mirrored layout. `ps3 decrypt` and `nds encrypt`/`decrypt` also
support it, single-file runs only.

### Cancellation

Pressing Ctrl-C stops the current operation cleanly. The running conversion aborts mid-file
at the next safe point, the partial output is removed, a `Cancelled` message is printed, and
the process exits with code 130. This covers every decrypt, encrypt, compress, and
decompress operation across all consoles. A pre-existing file chosen for overwrite is left
untouched, since the conversion writes to a temporary sibling and only renames it into place
once it finishes. In a batch run the file in progress is cancelled and the loop stops; files
already converted are kept. A cancelled run is reported with its own status rather than as a
failure.

Switch split and GoD conversion write multiple output files directly. Overwriting
these outputs is not atomic: failure or cancellation can remove replaced files.
Use a new output directory to retain an existing copy.

### Progress

A recursive `-R` run shows two bars: an overall one pinned on top with files done/total,
total size processed, and an ETA for the whole batch, and the per-file bar below it with
that file's throughput and remaining time. Single-file runs only show the per-file bar.

### Summaries

After `compress`, `decompress`, and `convert` operations the tool prints a closing summary
of bytes processed and space saved or expanded, for example
`12 files: 12.4 GiB -> 4.1 GiB, saved 8.3 GiB (67%) in 2m14s`. Verify and extract operations
print a file count and elapsed time instead.

GoD conversion prints a count summary rather than compression savings.

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
| `encrypt <INPUT> [OUTPUT]` | Encrypt a decrypted `.cia`, `.3ds`, `.cci`, or `.cxi` |
| `compress <INPUT> [OUTPUT]` | Compress a decrypted ROM to Z3DS |
| `decompress <INPUT> [OUTPUT]` | Decompress a Z3DS file back to the original ROM |
| `convert <INPUT> [OUTPUT]` | Convert between `.cia` and `.cci`/`.3ds`, direction auto-detected |
| `verify <INPUT>` | Verify `.cia` legitimacy or `.3ds`/`.cci` NCCH integrity |
| `info <INPUT>` | Inspect 3DS metadata. See [info](#info) |

Format-specific flags (shared conflict, recursion, template, and report flags are covered
in [Shared behaviors](#shared-behaviors)):

| Flag | Applies to | Description |
|---|---|---|
| `--output-dir <DIR>` | `cdn-to-cia`, `decrypt`, `encrypt`, `compress`, `decompress`, `convert` | Write outputs under this directory instead of beside each input |
| `-C, --cleanup` | `cdn-to-cia` | Remove original CDN files after conversion |
| `-T, --ensure-ticket-exists` | `cdn-to-cia` | Generate a ticket file if one is not found. The generated key is checked against the content, and the conversion fails with a clear error when it cannot decrypt instead of writing a broken CIA |
| `-D, --decrypt` | `cdn-to-cia` | Also decrypt the CIA after creation |
| `-Z, --compress` | `cdn-to-cia` | Also compress the CIA after creation (implies decrypt) |
| `-l, --level <LEVEL>` | `compress` | Zstd compression level 0..=22 (0 = library default, 22 = maximum ratio) |
| `--allow-encrypted` | `compress` | Compress even if the input ROM appears encrypted. By default an encrypted ROM is refused; decrypt first with `ctr decrypt` |
| `--full` | `verify` | Also verify content hashes against the TMD (CIA only, slower). `--verify-content` is an alias |

Generated tickets from `generate-cdn-ticket` use placeholder values and only work on modded
consoles and emulators. `cdn-to-cia` checks a generated ticket's key against the content and
fails with a clear error when it cannot decrypt, so supply the real ticket (cetk) for titles
whose key is not derivable. `decrypt` and `encrypt` support `.cia`, `.3ds`, `.cci`, and `.cxi`,
with the format detected automatically; place a `seeddb.bin` next to the executable to
resolve seeds locally, otherwise the seed is fetched from Nintendo's API. `encrypt` is the
inverse of the tool's decrypted output and rewrites CIA TMD hashes/content flags as it wraps
content with the ticket title key, so encrypted CIA bytes may differ from an original source
even when decrypting back to the same plaintext. `compress` inspects the crypto flags and
refuses an input that still looks encrypted, pointing you to `ctr decrypt`, unless you pass
`--allow-encrypted`.
`convert` produces an unsigned CIA with a zero title key, compatible with CFW and emulators
but not installable on stock hardware.

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
| `--key <KEYFILE>` | `compress`, `verify` | Disc master key file for `.wud`/`.wux` inputs (optional). Pass once per disc input in positional order on `compress`. See key resolution below |

`wup` commands do not take `--output-dir` or `--output-template`, because `compress` packs
many inputs into a single archive and `decrypt` writes a directory tree. `compress` and
`verify` auto-detect each input as a loadiine directory, a NUS directory, or a disc image.

For disc images (`.wud`/`.wux`), the disc key is optional. It is resolved in order:
explicit `--key`, else a sibling `<disc>.key` or `game.key` file, else the built-in disc key
database matched by the input filename, else automatic probe of the built-in database against
the disc, else an error if none match. `--key` cannot be combined with `--recursive`, since
one key can't be right for every disc in a batch.

`decrypt` handles both the canonical Nintendo layout (`title.tmd` + `title.tik` + `{id}.app`)
and the community layout variant (`tmd.<N>` + optional `cetk.<N>` + extensionless content files);
when no ticket is present the title key is derived from the title id.

## nx (Nintendo Switch)

```sh
rom-converto nx <SUBCOMMAND> <INPUT>
```

| Subcommand | Description |
|---|---|
| `compress <INPUT> [-o OUTPUT]` | Compress NSP to NSZ or XCI to XCZ |
| `decompress <INPUT> [-o OUTPUT]` | Restore NSZ/XCZ to NSP/XCI |
| `merge <INPUT>... [-o OUTPUT]` | Combine base, update, or DLC containers into one NSP or XCI |
| `split <INPUT> [--output-dir DIR]` | Write one NSP per title from an NSP or XCI |
| `verify <INPUT>` | Check NCA hashes, including in compressed containers |
| `info <INPUT>` | Inspect container metadata. See [info](#info) |

| Flag | Applies to | Description |
|---|---|---|
| `--keys <PRODKEYS>` | all | Use this key file. Otherwise search the user's `.switch/prod.keys`, then beside the executable. See [key files](configuration.md#key-files) |
| `-l, --level <LEVEL>` | `compress` | Zstd level 1..=22; default 18 |
| `--mode <MODE>` | `compress` | `solid` (one zstd frame per NCA, default for NSP) or `block` (default for XCI) |
| `--block-size-exp <EXP>` | `compress` | Block size is `1 << exp` bytes; range 14..=32, default 20 (1 MiB) |
| `--output-dir <DIR>` | `compress`, `decompress`, `split` | Choose the output directory |
| `--format <nsp\|xci>` | `merge` | Default NSP. XCI output requires every input to be an XCI |
| `--on-conflict <POLICY>`, `-f, --force` | `merge`, `split` | Default `error`; `-f` means overwrite. The flags conflict |

A valid `prod.keys` is required and is never modified. Merge and split accept only
uncompressed NSP/XCI inputs; decompress NSZ/XCZ first.

### Merge and split

Merge keeps the highest CNMT version for each title and content type, then deduplicates
the selected NCAs. NSP output includes tickets and certificates; XCI output includes
NCAs only. Other sidecars are dropped. Split always writes NSPs, named from the input
stem, title ID, version, and an update/DLC marker where applicable. It cannot restore
versions or files discarded by a merge.

Merge targets emulator use and always displays a signature warning. Selected NCA
bytes are copied unchanged; XCI output has newly generated, unsigned gamecard headers.

Default outputs:

- Merge: `<first stem> (Merged).nsp` or `.xci`, beside the first input.
- Split: a `<stem>_split` directory beside the input.
- The configured `[nx].output_dir`, including presets, overrides those directories.
  An explicit `-o` (merge) or `--output-dir` (split) takes precedence.

Neither command supports recursion, reports, output templates, or a positional output.
Merge has no `--output-dir` flag; use `-o`. Configured conflict policies are not used:
set `--on-conflict` or `-f` on the command.

`overwrite-invalid` acts as `skip` because these commands do not verify existing
outputs. Split rejects `rename` in the CLI. Its overwrite mode reuses the directory,
replacing planned NSP files while leaving unrelated files in place.

```sh
rom-converto nx merge base.nsp update.nsp dlc.nsp --keys prod.keys -o merged.nsp
rom-converto nx merge base.xci update.xci --format xci -o merged.xci
rom-converto nx split merged.nsp --output-dir ./titles
```

## nds (Nintendo DS)

```
rom-converto nds <SUBCOMMAND> <INPUT> [OUTPUT]
```

| Subcommand | Description |
|---|---|
| `encrypt <INPUT> [OUTPUT]` | Encrypt a decrypted Nintendo DS ROM's KEY1 secure area |
| `decrypt <INPUT> [OUTPUT]` | Decrypt an encrypted Nintendo DS ROM's KEY1 secure area |
| `info <INPUT>` | Report the header fields, header CRC16, secure-area encryption state, banner titles, and 32x32 icon of a `.nds`/`.dsi` ROM |

| Flag | Applies to | Description |
|---|---|---|
| `--output-dir <DIR>` | `encrypt`, `decrypt` | Write outputs under this directory instead of beside each input |
| `--output-template <TEMPLATE>` | `encrypt`, `decrypt` | Output path template for single-file runs. See [Output-path templates](#output-path-templates) |
| `-R, --recursive` | `encrypt`, `decrypt` | Process every `.nds` found in `INPUT` and its subdirectories |
| `--max-depth <N>` | `encrypt`, `decrypt` | Maximum directory depth under `--recursive`. 1 = top level only |
| `--report <FILE>` | `encrypt`, `decrypt` | Write a run report. See [Run reports](#run-reports) |

Both commands auto-detect whether the ROM's secure area is already in the requested state
and rewrite only the first 0x800 bytes of the KEY1 secure area at offset 0x4000. A homebrew
ROM with no secure area, and a ROM already in the requested state, are skipped rather than
treated as an error. DSi-enhanced carts get their KEY1 secure area processed the same way;
the DSi modcrypt region is left untouched.

## chd (CD / DVD / LaserDisc)

```
rom-converto chd <SUBCOMMAND> <INPUT> [OUTPUT]
```

| Subcommand | Description |
|---|---|
| `compress <INPUT> [OUTPUT]` | Compress a `.cue`, `.iso`, or `.avi` to `.chd`; CD, DVD, and LaserDisc media are auto-detected |
| `migrate <INPUT> [OUTPUT]` | Rewrite a CHD v1 to v4 as v5; defaults to `<name>.v5.chd` |
| `extract <INPUT> [OUTPUT]` | Extract a `.chd` back to `.bin` + `.cue` (CD) or `.iso` (DVD); LaserDisc CHDs are not supported |
| `verify <INPUT>` | Verify SHA-1 integrity of a v5 CHD; migrate older versions first |
| `to-cso <INPUT> [OUTPUT]` | Extract a DVD-mode `.chd` straight to `.cso` (default) or `.zso`, through a temporary ISO |
| `info <INPUT>` | Inspect CHD v1 to v5 metadata and stored hashes. See [info](#info) |

| Flag | Applies to | Description |
|---|---|---|
| `--dvd` / `--cd` / `--ld` | `compress` | Override the auto-detected mode (CD mode needs a cue sheet, LD mode needs a `.avi`) |
| `--hunk-size <BYTES>` | `compress`, `migrate` | DVD compression uses a multiple of 2048, defaulting to 4096 (2048 for detected PSP images). Migration inherits the source size; overrides must be a nonzero multiple of its unit size |
| `-c, --codecs <LIST>` | `compress`, `migrate` | Up to four comma-separated codecs: `zlib`, `zstd`, `lzma`, `huff`, `flac`, `cdzl`, `cdzs`, `cdlz`, `cdfl` |
| `-l, --level <LEVEL>` | `compress`, `migrate` | Compression level 1..=22. Zlib and LZMA cap at 9 |
| `--in-place` | `migrate` | Replace the source with its v5 conversion. Conflicts with explicit output paths, `--output-dir`, and `--output-template` |
| `--format <cso\|zso>` | `to-cso` | Output container: CSO for PSP/PPSSPP, ZSO for PS2 via Open PS2 Loader |
| `--block-size <BYTES>` | `to-cso` | Block size, a power of two; defaults to 2048 (16384 for 2 GiB+ inputs) |
| `--output-dir <DIR>` | `compress`, `migrate`, `extract`, `to-cso` | Write outputs under this directory instead of beside each input |
| `-p, --parent <PARENT>` | `extract`, `verify` | Specify a parent CHD for parent-child relationships |
| `--fix` | `verify` | Correct SHA-1 values in the CHD header if mismatches are found |

`compress` probes the media type from the image: a `.cue` is CD mode, an `.iso` is CD or DVD
mode depending on the detected console, and an `.avi` is LD mode, the `createld` equivalent.
The createcd/createdvd mixup cannot happen. LD mode always writes an avhuff-compressed CHD and
does not accept `--codecs`, `--level`, or `--hunk-size`, since the codec and per-field hunk
size are fixed by the AVI. The input must be uncompressed 4:2:2 video (YUY2, UYVY, or VYUY)
with 8- or 16-bit PCM audio; a compressed video codec such as HuffYUV is rejected, naming the
codec found. Extract report rows carry zero byte sizes since extraction writes several files.

`migrate` preserves decoded data while rebuilding the container. It supports legacy
zlib, uncompressed, and audio/video CHDs, including hard-disk images. Parent-dependent
CHDs, unsupported map entries, and files already at v5 are rejected. Audio/video
migration does not write AVHUFF, so the output may be much larger.

Migration inherits the source hunk and unit sizes. Defaults use CD codecs for 2448-byte
units and general codecs otherwise; CD-only codecs require 2448-byte units. Metadata
is copied, with two exceptions: v1/v2 disk geometry becomes `GDDD` metadata, and old
`CHTR` track entries become `CHT2` when no `CHCD` metadata is present.

Migration checks legacy map data and available hunk CRCs, but does not validate the
source's stored MD5 or SHA-1. `extract`, `verify`, and `to-cso` require v5 input.

Current CLI migration limits:

- `--in-place` always replaces the source, even with `--on-conflict skip`.
- With `-R`, use `--output-dir` to mirror the input folders elsewhere. Explicit output
  paths, `--output-template`, `--on-conflict`, and `--report` are ignored.
- Recursive runs keep going after a file fails. Existing outputs fail that item unless
  `-f` or `--in-place` is set.

```sh
rom-converto chd migrate old.chd
rom-converto chd migrate -R ./chd --output-dir ./v5 --dry-run
```

`to-cso` only accepts a DVD-mode CHD (PS2 DVD, PSP UMD); a CD-mode or LD-mode CHD has no flat
ISO for CSO/ZSO to hold, and is rejected up front. It extracts to a temporary ISO next to the
output, runs the same CSO/ZSO writer `cso compress` uses, and always removes the temporary ISO
afterward, whether the run succeeds, fails, or is cancelled.

`extract` does not support LaserDisc CHDs yet; it errors out naming the limitation rather than
writing a partial file.

`info` on an LD-mode CHD prints an LD block (fps, field size, interlacing, audio, frame count)
decoded from the `AVAV` metadata, plus a VBI summary (CAV picture numbers, CLV timecodes,
chapters, white flags, lead-in/out) decoded from the `AVLD` metadata. Running `info` on a
LaserDisc rip's `.avi` directly prints the same container and VBI report before compression;
see [info](#info).

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
| `-c, --codecs <LIST>` | `to-chd` | Up to four comma-separated CHD codecs. Uses the same values and defaults as `chd compress` |
| `-l, --level <LEVEL>` | `to-chd` | Compression level 1..=22. Zlib and LZMA cap at 9 |
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
rom-converto cue <SUBCOMMAND> <INPUT> [OUTPUT]
```

| Subcommand | Description |
|---|---|
| `merge <INPUT_CUE> <OUTPUT_CUE>` | Merge a multi-bin `.cue` into a single `.bin` + `.cue` pair |
| `to-iso <INPUT> [OUTPUT]` | Convert a `.cue`/`.bin` disc image's data track to a plain `.iso` |
| `to-cso <INPUT> [OUTPUT]` | Convert a `.cue`/`.bin` disc image's data track straight to `.cso`/`.zso` |

| Flag | Applies to | Description |
|---|---|---|
| `--output-dir <DIR>` | `to-iso`, `to-cso` | Write outputs under this directory instead of beside each input |
| `--format <cso\|zso>` | `to-cso` | Output container: CSO for PSP/PPSSPP, ZSO for PS2 via Open PS2 Loader (default `zso`) |
| `-R, --recursive` | `to-iso`, `to-cso` | Convert every `.cue` found in `INPUT` and its subdirectories |
| `--max-depth <N>` | `to-iso`, `to-cso` | Maximum directory depth under `--recursive`. 1 = top level only |

`merge` combines one `.bin` per track (for emulators that cannot load split images) into a
single `.bin` named after the output `.cue`. It takes `--on-conflict` (and `-f`) only; the
`.bin` sidecar follows the renamed `.cue`.

`to-iso` extracts the first track, which must be a MODE1/MODE2 data track, to 2048-byte ISO
sectors; any audio tracks are skipped. `to-cso` extracts the data track to a temporary ISO,
compresses it, and always removes the temporary ISO afterward.

## ps3 (PlayStation 3)

```
rom-converto ps3 <SUBCOMMAND> <INPUT> [OUTPUT]
```

| Subcommand | Description |
|---|---|
| `decrypt <INPUT> [OUTPUT]` | Decrypt an encrypted PS3 ISO into a plain ISO |
| `info <INPUT>` | Inspect PS3 disc metadata. See [info](#info) |

| Flag | Applies to | Description |
|---|---|---|
| `--key <FILE>` | `decrypt` | Disc data key file (`.dkey`). Optional, see key resolution below. Can't be combined with `--recursive` |
| `--skip-probe` | `decrypt` | Skip the encryption and key verification probe (use if a correct key is rejected) |
| `--output-dir <DIR>` | `decrypt` | Write outputs under this directory instead of beside each input |
| `--output-template <TEMPLATE>` | `decrypt` | Output path template for single-file runs. See [Output-path templates](#output-path-templates) |
| `-R, --recursive` | `decrypt` | Decrypt every `.iso` found in `INPUT` and its subdirectories |
| `--max-depth <N>` | `decrypt` | Maximum directory depth under `--recursive`. 1 = top level only |
| `--report <FILE>` | `decrypt` | Write a run report. See [Run reports](#run-reports) |

The disc alternates plain and encrypted sector regions; encrypted regions are AES-128-CBC
decrypted with the per-disc data key. Output covers the region table's sector span, so
trailing padding past it is not copied.

The data key is resolved in order: explicit `--key`, else the built-in disc key database
looked up by the disc's title ID, else a sibling `<input>.dkey`. `--key` cannot be combined
with `--recursive`, since one key can't be right for every disc in a batch.

`info --save-icon <DIR>` writes the `ICON0.PNG` icon as `<title_id>.png`; no key is needed
to read it.

## xbox (Original Xbox)

```
rom-converto xbox <SUBCOMMAND> <INPUT> [OUTPUT]
```

| Subcommand | Description |
|---|---|
| `convert <INPUT> [OUTPUT]` | Convert a full disc image or a game directory to a trimmed `.xiso` |
| `extract <INPUT> <OUTPUT_DIR>` | Extract a `.xiso` or full disc image to files |
| `info <INPUT>` | Inspect Xbox disc layout. See [info](#info) |

| Flag | Applies to | Description |
|---|---|---|
| `-o, --output <FILE>` | `convert` | Output file path |
| `--no-media-patch` | `convert` | Skip XBE media patching, which runs by default |

`convert` accepts a full disc image (`.iso`) or an extracted game directory, and rebuilds the
disc layout so video-partition padding is removed. Takes `-f`/`--force` and the shared conflict
policy. Output works in xemu and on modded consoles.

`info` prints layout details: XGD1, XGD2, XGD3, or trimmed, plus file counts and sizes. When
a root `default.xbe` is present, it also prints the XBE title metadata (title ID, name,
version, disc number, region, allowed media, ratings). A root `default.xex` (a 360 XDVDFS
disc routed here) prints XEX title metadata instead. `--save-icon <DIR>` writes the XBE title
image as `<title_id_hex>.png`, falling back to the XEX icon when only a `default.xex` is
present.

## xenon (Xbox 360)

```sh
rom-converto xenon <SUBCOMMAND> <INPUT>
```

| Subcommand | Description |
|---|---|
| `compress <INPUT> [OUTPUT]` | Pack a disc ISO or extracted game folder into ZAR |
| `convert <INPUT> [OUTPUT_DIR]` | Convert a disc ISO to an unsigned Games on Demand install tree |
| `extract <INPUT> <OUTPUT_DIR>` | Extract ZAR files |
| `verify <INPUT>` | Verify a ZAR archive |
| `info <INPUT>` | Inspect Xbox 360 disc or archive metadata. See [info](#info) |

`compress` writes ZAR directly from the disc without extracting it first, using zstd
across CPU cores. The archive puts `default.xex` at its root for Xenia Canary.

`info` reads title ID, name, version, disc number, region, allowed media, and the icon
from `default.xex` when present. `--save-icon <DIR>` saves the icon as `<title_id_hex>.png`.

### Convert to GoD

| Flag | Description |
|---|---|
| `--title <NAME>` | Override the display title read from `default.xex` |
| `--on-conflict <POLICY>` | Choose how to handle an existing output directory |
| `-f, --force` | Overwrite; conflicts with `--on-conflict` |

`convert` requires a disc ISO with a root `default.xex`; extracted folders and archives
are not accepted by the CLI. The optional output directory is positional. There is no
`-o`, recursion, output-template, report, or GoD verification option. No key file is
required. The default output is `<stem>_god` beside the input.

The output has a header at `<TITLEID>/00007000/<MEDIAID>` and parts under
`<MEDIAID>.data/DataNNNN`. It trims unused disc data and adds hashes, without compression.
The header is unsigned and intended for modified consoles.

An existing non-empty directory fails by default. `skip` and `overwrite-invalid`
skip existing directories because GoD has no integrity probe. CLI `rename` is
unsupported. Overwrite replaces the matching title/media files, preserving other
titles in the tree.

```sh
rom-converto xenon convert game.iso
rom-converto xenon convert game.iso ./god --title "Game title" --dry-run
```

## psp (PSP EBOOT.PBP)

```
rom-converto psp info <INPUT> [--json] [--save-icon DIR]
rom-converto psp extract <INPUT> <OUTPUT_DIR>
rom-converto psp to-iso <INPUT> [OUTPUT] [--output-template TEMPLATE]
```

`info` on an `EBOOT.PBP` reports the `PARAM.SFO` fields, the embedded icon, the segment
layout, and what `DATA.PSAR` holds (`NPUMDIMG` is an encrypted PSN image). `extract` writes
each present segment (`PARAM.SFO`, `ICON0.PNG`, ..., `DATA.PSAR`) into `OUTPUT_DIR` under
its standard name; `DATA.PSAR` is written as stored, so it stays encrypted for an
`NPUMDIMG` image. `to-iso` decrypts an `NPUMDIMG` EBOOT into a plain ISO, and also accepts
a PSN `.pkg` directly, reading the EBOOT out of the decrypted package. `OUTPUT` defaults to
`<INPUT>.iso`; `--output-template` takes the same tokens as the other commands.

## vita (PS Vita)

```
rom-converto vita info <INPUT> [--json] [--save-icon DIR]
rom-converto vita extract <INPUT> <OUTPUT_DIR>
```

`info` reads a `.vpk` (title, title ID, content ID, category, item counts, bubble icon) or
a PSN `.pkg`; `.pkg` coverage including the PSP and PS3 variants is described under
[info](#info). `extract` decrypts a PKG with its embedded key index and writes every file
item into `OUTPUT_DIR`, keeping the paths from the item table. Vita file items stay
PFS-encrypted as stored; the package structure and plaintext items extract as-is.

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
| `--algo <ALGOS>` | `scan` | Same values. Default `crc32`: size plus CRC32 identifies almost everything, so scans stay fast; raise it when a match needs a stronger digest |
| `--input-checksum-min <ALGO>` | `verify`, `identify` | Checksum tier always computed before consulting Playmatch: `crc32`, `md5`, `sha1`, or `sha256`. Default `crc32` |
| `--input-checksum-max <ALGO>` | `verify`, `identify` | Ceiling on how far escalation may go past the floor tier. Default `sha256` |
| `-R`, `--recursive` | `verify`, `rename` | Process every file under INPUT, descending into subdirectories |
| `--max-depth <N>` | `verify`, `scan`, `rename`, `fixdat` | Limit recursion depth. `1` = top level only. On `verify` and `rename` requires `-R`; `scan` and `fixdat` always walk the whole directory |
| `--report <FILE>` | `verify`, `scan`, `rename` | Write a run report. See [Run reports](#run-reports) |
| `--quick` | `verify`, `scan` | Trust a zip's own CRC32 for an eligible cartridge image instead of extracting and hashing it. Falls back automatically when the archive checksum alone does not verify |
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

`verify` and `identify` compute `--input-checksum-min` first (`crc32` by default) and only
compute the rest of `--algo`'s digest set if that floor tier alone does not resolve a
verified match, up to `--input-checksum-max`. This is skipped for compressed containers
(`.chd`, `.rvz`, `.wbfs`, `.cso`, `.zso`, GCZ, WIA, NKit, Z3DS): their decode already
dominates cost, so all requested digests are computed together in the one pass instead of
risking a second decode. Raw files and cue-set `.bin` tracks benefit the most, since a
confident crc32 match skips the extra digests entirely. The hash cache remembers whichever
tier was last computed, so escalation happens at most once per file across a run.
`--algo` may not request a digest stronger than `--input-checksum-max`; that combination
is rejected at startup instead of silently skipping the digest.

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
rom-converto info <INPUT> [--json] [--save-icon DIR] [--keys FILE]
```

Inspect embedded metadata. The generic `info` command auto-detects the format from the
extension and, for shared disc extensions, header data. Console-specific `info` commands
read the same metadata directly.

Text output uses `Container`, `ROM`, and `Inner files` sections when relevant. `--json`
returns the same data as structured JSON. Encrypted 3DS CIA headers are read in memory.

| Flag | Description |
|---|---|
| `--json` | Emit a machine-readable payload instead of the formatted report |
| `--batch` | Generic `info` only. Recursively inspect every supported file under directory `INPUT` instead of reading it as one title |
| `--paths-file <FILE>` | Generic `info` only. Read one input path per line; skip blank lines and `#` comments. Conflicts with `INPUT` |
| `--save-icon <DIR>` | Write embedded artwork to `DIR`. Supported for `ctr`, `dol`, `rvl`, `nx`, `wup`, `xbox`, `xenon`, `ps3`, PSP images, VPK, and PKG |
| `--keys <FILE>` | `prod.keys` for `nx info`, a disc master key file for `wup info` on `.wud`/`.wux` (optional), or a `.dkey` file for `ps3 info`. Other consoles do not use it |

Supported inputs include every console and format listed in [Formats](formats.md), plus
common cartridge ROMs: NES, SNES, Nintendo 64, Game Boy, Game Boy Color, Game Boy Advance,
Mega Drive, Master System, Game Gear, Virtual Boy, WonderSwan, Neo Geo Pocket, Atari Lynx,
and Atari 7800. It also reads PS1, PS2, and PSP discs; PSP, PS3, and Vita PKG files;
`.3dsx` and `.z3dsx`; and LaserDisc AVI files. NFS and TGC are unsupported.

`chd info` and `cso info` probe an inner PS1, PS2, or PSP disc when possible. A failed
probe does not hide the outer container details. `--save-icon` is unavailable for CHD,
CSO, PS1, PS2, retro cartridge, and LaserDisc inputs.

## capabilities

```
rom-converto capabilities
```

Print the operations and info extensions the installed binary supports as JSON, for
frontends and scripts that need to know what this build can do before calling it. The
payload carries the supported operations, the full `info` extension list, and the JSON
runner schema.

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

# Desktop GUI

The desktop app runs the same operations as the CLI over the same library, so a GUI run and
the equivalent CLI command produce identical output. It is built with
[Tauri](https://tauri.app/), [Nuxt](https://nuxt.com/), and
[Tailwind CSS](https://tailwindcss.com/), and runs on Windows, macOS, and Linux.

The app adds drag-and-drop batch queues, live progress, a cancel button, per-page option
controls, and a rich info card. It adds no conversion capability the CLI lacks. The command
line echoed above each result shows the equivalent CLI invocation for the options you chose.

## Pages

Each console or format family has its own set of pages, one per operation, reachable from
the sidebar:

| Family | Pages |
|---|---|
| 3DS | CDN to CIA, Decrypt ROM, Compress to Z3DS, Decompress Z3DS, Verify 3DS ROM, Convert CIA/CCI, Generate ticket, 3DS info |
| GameCube | Compress to RVZ, Decompress RVZ, Verify GameCube disc, GameCube info |
| Wii | Compress to RVZ, Decompress RVZ, Verify Wii disc, Wii info |
| Wii U | Compress to WUA, Decrypt NUS title, Verify Wii U title, Wii U info |
| Switch | Compress to NSZ/XCZ, Decompress NSZ/XCZ, Verify Switch container, Switch info |
| CHD | Compress to CHD, Extract CHD, Verify CHD, CHD info |
| CSO/ZSO | Compress to CSO/ZSO, Decompress CSO/ZSO, Verify CSO/ZSO, CSO/ZSO info |
| CD (CUE/BIN) | Merge multi-bin |
| Utilities | Hash, Playlist |
| DAT | Verify, Scan, Rename |

## CLI and GUI parity

Every GUI control forwards to the same library function the CLI uses. The table maps each
CLI command to its GUI page.

| CLI command | GUI page |
|---|---|
| `chd compress`, `extract`, `verify`, `info` | CHD |
| `cso compress`, `decompress`, `verify`, `info` | CSO/ZSO |
| `ctr` (all operations) | 3DS |
| `dol compress`, `decompress`, `verify`, `info` | GameCube |
| `rvl compress`, `decompress`, `verify`, `info` | Wii |
| `wup compress`, `decrypt`, `verify`, `info` | Wii U |
| `nx compress`, `decompress`, `verify`, `info` | Switch |
| `cue merge` | Merge multi-bin |
| `hash` | Utilities: Hash |
| `playlist` | Utilities: Playlist |
| `dat verify`, `scan`, `rename` | DAT |

A few CLI features have no GUI counterpart by design:

- `shell-completions` and `self-update` are terminal and Tauri concerns; the desktop app
  updates itself through Tauri.
- `-v`/`--verbose`, `--debug-log`, and `-q`/`--quiet` are terminal logging controls; the
  GUI shows operation output in its own log panel.
- `--config`, `--preset`, and `--no-update-check` are file-based configuration and updater
  flags; the GUI keeps options per page.
- `info --json` is scripting output for the terminal; the GUI shows a rich info card.
- `dat identify` and `dat fixdat` are terminal operations: a single-file database lookup and
  a Logiqx fixdat builder. The GUI covers `dat verify`, `scan`, and `rename`.

## Conflict control

Pages that write output expose an "On conflict" control with Overwrite, Skip, Rename, Error,
and Overwrite if invalid. The choice is resolved before the write, so Skip and Error never
touch an existing file. Overwrite if invalid runs the same per-format integrity check the CLI
does before deciding keep versus rewrite, and falls back to existence-based skip for outputs
that have no integrity check. On the Wii U decrypt page Rename is disabled, because the output
is a directory.

## Preview mode

Most write-capable pages have a Preview toggle. Turning it on makes the Run button preview the
plan instead of running it: one plan line per file in the same
`Would <op> <in> -> <out> (<FMT>) [<decision>]` form as the CLI `--dry-run`, with nothing
written. The preview shares the CLI's plan logic through the library, so a preview line matches
the CLI's line for the same input. Recursive CDN to CIA is the one write page without a
preview, mirroring how the CLI special-cases that batch. The DAT Rename page has its own
Preview toggle that lists each file's planned rename rather than a conversion plan line.

## Cancellation

A Cancel button aborts the running conversion immediately, both for a single file and within
a batch, and discards the partial output it created. Cancellation covers every decrypt,
encrypt, compress, and decompress operation across all consoles. A file chosen for overwrite
is left untouched, and a cancelled run is reported with its own status rather than as a
failure.

## Recursive folder scan

Dropping or browsing a folder onto a write-capable batch page scans it for matching input
files and queues them, using the same junk-filtered library walk as the CLI. A Recursive
toggle with an optional max-depth controls how deep the scan goes, defaulting to full depth
like the CLI `-R`.

## Disk-space preflight

Before any write-producing command runs, the GUI checks the output filesystem for free space,
using the total size of the input files as a conservative floor plus a 256 MiB headroom. If
space looks insufficient it reports an error and writes nothing, naming the directory, the
estimated need, and the space available. The estimate is a floor and cannot account for
decompression that expands well beyond its input. If the free-space query fails, the run
proceeds. The "Skip free space check" toggle on each write page bypasses the guard, matching
the CLI's `--skip-space-check`.

## Output templates and reports

The GUI exposes `--report` on the compress and decompress pages for dol, rvl, cso, and nx,
plus chd compress and chd extract. It accumulates one record per processed file and calls the
same report library function the CLI uses, so the CSV, JSON, and HTML output is identical. The
report file is overwritten directly and does not go through the on-conflict control.

The GUI also exposes an output template field on those pages plus the four CTR operations
(compress, decompress, convert, decrypt). For CTR the template is single file only, so the
field is hidden once files are queued for a batch. The template resolves through the same
library functions as the CLI, so the resolved path matches. An output template and an explicit
output path are mutually exclusive: entering a template disables the explicit output field.

## Option gating

The GUI disables options that do not apply to the current selection and explains why with a
tooltip. The Run button is disabled, with a tooltip giving the exact reason, whenever the
configuration cannot run, for example when no input is selected, when a queue is empty, or when
an output template field is set but blank.

## Configuration

The GUI does not read the config file. Options are set per page in the app. The CLI is the
path for config-driven presets; see [`configuration.md`](configuration.md).

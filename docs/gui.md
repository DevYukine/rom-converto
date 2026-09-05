# Desktop GUI

The desktop app runs the same operations as the CLI over the same library, so a GUI run and
the equivalent CLI command produce identical output. It is built with
[Tauri](https://tauri.app/), [Nuxt](https://nuxt.com/), and
[Tailwind CSS](https://tailwindcss.com/), and runs on Windows, macOS, and Linux.

The app adds drag-and-drop batch queues, live progress, a cancel button, per-page option
controls, and a rich info card. It adds no conversion capability the CLI lacks. The command
line echoed above each result shows the equivalent CLI invocation for the options you chose.

## Batch queue

Batch-capable pages group queued files into three sections: Active (pending and running),
Completed, and Failed. Each running file shows its own progress bar and elapsed time. Pending
files can be dragged to reorder them; running and finished files keep their position. A
"Concurrent jobs" control sets how many files convert at once (default 2, up to 8); this is
separate from any per-conversion thread or worker-pool setting a page exposes. Checkboxes let
you select files across sections for "Remove selected". A "Retry failed" button re-runs only
the files left in the Failed section, without restarting files that already finished. Wii U
compress is a bundle operation (every queued title is packed into one output archive), so it
shows the same grouped list but without reorder, selection, retry, or the concurrency control.

## Pages

Each console or format family has its own set of pages, one per operation, reachable from
the sidebar:

| Family | Pages |
|---|---|
| Nintendo DS | Encrypt ROM, Decrypt ROM |
| 3DS | CDN to CIA, Decrypt ROM, Compress to Z3DS, Decompress Z3DS, Verify 3DS ROM, Convert CIA/CCI, Generate ticket, 3DS info |
| GameCube | Compress to RVZ, Decompress RVZ, Verify GameCube disc, GameCube info |
| Wii | Compress to RVZ, Decompress RVZ, Verify Wii disc, Wii info |
| Wii U | Compress to WUA, Decrypt NUS title, Verify Wii U title, Wii U info |
| Switch | Compress to NSZ/XCZ, Decompress NSZ/XCZ, Verify Switch container, Switch info |
| CHD | Compress to CHD, Migrate CHD to v5, Extract CHD, Extract to CSO/ZSO, Verify CHD, CHD info |
| CSO/ZSO | Compress to CSO/ZSO, Decompress CSO/ZSO, Compress to CHD, Verify CSO/ZSO, CSO/ZSO info |
| CD (CUE/BIN) | Merge multi-bin, Convert CUE/BIN |
| Xbox | Convert ISO → XISO, Extract XISO, Xbox info |
| Xbox 360 | Compress to ZAR, Convert ISO to GoD, Extract ZArchive, Verify Xbox 360, Xbox 360 info |
| PlayStation 3 | Decrypt ISO, PS3 info |
| Utilities | Hash, Playlist, Merge Switch NSP/XCI, Split Switch NSP/XCI, Settings |
| DAT | Verify, Scan, Rename |

Compress to CHD (under CSO/ZSO) and Extract to CSO/ZSO (under CHD) each run as a single
conversion job: the intermediate ISO is written to a temp path, converted, and removed
automatically, with one progress bar and one comparison card for the whole job rather than
two separate runs.

Compress to CHD accepts `.cue`, `.iso`, and `.avi` and auto-detects CD, DVD, or LD mode from
the input, the same as the CLI; a mode picker lets you override it. Picking or auto-detecting
LD mode hides the codec, level, and hunk-size fields, since LD-mode CHDs always use avhuff
with a per-field hunk size fixed by the AVI.

Migrate CHD to v5 rewrites a CHD in format version 1 to 4 as a version 5 CHD. The decoded raw
stream is copied through unchanged, so only the container and the compression are rebuilt.
Metadata is kept verbatim except for the pre-2009 `CHTR` track entries, which become `CHT2`
the way `chdman copy` does so the overall SHA-1 matches current chdman output. Hunk size defaults to the source's, and the codecs default to the CD
or DVD set matching the source's unit size. Input and output share the `.chd` extension, so the
output is written next to the source as `<name>.v5.chd` instead of over it. A CHD that is
already version 5 is rejected. The CLI's `--in-place` has no GUI equivalent on purpose:
queued jobs always write to a path you can see before they run.

The Inspect page (reached from the sidebar's Inspect icon, not a per-console page) reads any
supported file the same way the CLI's `rom-converto info` does. The card lays out Container,
ROM, inner files, and hashes as a fixed grid, so each lands in the same spot regardless of
console. A content-type chip next to the title colors Game, Update, DLC, and Demo
differently, and multi-language fields such as titles, publishers, and age ratings show
their resolved display name rather than a raw code. The icon and, for a PSP disc, its
background art render above the grid; dropping a laserdisc rip's `.avi` shows its
container info and, for uncompressed video, a VBI summary. Dropping a PS1 or PS2 image
(`.iso`, `.cue`) or a PSP `.iso` shows its disc kind and normalized title ID. Inspecting a CHD or CSO/ZSO also shows
the PlayStation disc it contains, when one is detected, alongside the container's own info.
The keys path (`prod.keys` for Switch, an optional disc master key for Wii U) is entered
once on this page and persists across files and app restarts.

The Merge Switch NSP/XCI page combines every staged file into one queue job producing a
single super container, and shows a persistent warning above the drop zone: merged containers
fail signature verification; CFW installers reject them unless signature checks are disabled
(not advised). They are intended for emulators. With no output picked it
writes `<first input's name> (Merged).nsp` (or `.xci`) next to the first input.

The Split Switch NSP/XCI page writes one `.nsp` per title. With no output directory picked
it uses `<name>_split` next to the input.

## CLI and GUI parity

Every GUI control forwards to the same library function the CLI uses. The table maps each
CLI command to its GUI page.

| CLI command | GUI page |
|---|---|
| `nds encrypt`, `decrypt` | Nintendo DS |
| `chd compress`, `migrate`, `extract`, `to-cso`, `verify`, `info` | CHD |
| `cso compress`, `decompress`, `to-chd`, `verify`, `info` | CSO/ZSO |
| `ctr` (all operations) | 3DS |
| `dol compress`, `decompress`, `verify`, `info` | GameCube |
| `rvl compress`, `decompress`, `verify`, `info` | Wii |
| `wup compress`, `decrypt`, `verify`, `info` | Wii U |
| `nx compress`, `decompress`, `verify`, `info` | Switch |
| `cue merge` | Merge multi-bin |
| `cue to-iso`, `to-cso` | Convert CUE/BIN |
| `xbox convert`, `extract`, `info` | Xbox |
| `xenon compress`, `convert`, `extract`, `verify`, `info` | Xbox 360 |
| `ps3 decrypt`, `info` | PlayStation 3 |
| `hash` | Utilities: Hash |
| `playlist` | Utilities: Playlist |
| `nx merge` | Utilities: Merge Switch NSP/XCI |
| `nx split` | Utilities: Split Switch NSP/XCI |
| `dat verify`, `scan`, `rename` | DAT |

A few CLI features have no GUI counterpart by design:

- `shell-completions` and `self-update` are terminal and Tauri concerns; the desktop app
  updates itself through Tauri. It checks shortly after launch and every four hours, shows a
  small notice in the bottom right when a release is available, and only installs when you
  choose to. Later hides the notice until the next launch, Skip hides that version for good,
  and Settings can turn the automatic check off.
- `-v`/`--verbose`, `--debug-log`, and `-q`/`--quiet` are terminal logging controls; the
  GUI shows operation output in its own log panel.
- `--config`, `--preset`, and `--no-update-check` are file-based configuration and updater
  flags; the GUI keeps options per page.
- `info --json` is scripting output for the terminal; the GUI shows a rich info card.
- `dat identify` and `dat fixdat` are terminal operations: a single-file database lookup and
  a Logiqx fixdat builder. The GUI covers `dat verify`, `scan`, and `rename`. Scan streams each
  file's match outcome live as it is processed and can be cancelled mid-run, keeping the
  partial results shown so far on screen.
- `dol migrate` and `rvl migrate` have no dedicated page. The Compress to RVZ pages accept
  legacy GCZ, WIA (Wii only), and NKit inputs and migrate them automatically, verifying the
  source first; migrate's CLI-only `--skip-verify` and `--deep` knobs have no GUI control.

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
a batch, and discards the partial output it created. When a batch runs more than one
concurrent job, Cancel stops every in-flight job, not just one; a partially-completed batch
keeps the files it already finished in the Completed section. Cancellation covers every
decrypt, encrypt, compress, and decompress operation across all consoles. A file chosen for
overwrite is left untouched, and a cancelled run is reported with its own status rather than
as a failure.

## Recursive folder scan

Dropping or browsing a folder onto a write-capable batch page scans it for matching input
files and queues them, using the same junk-filtered library walk as the CLI. A Recursive
toggle with an optional max-depth controls how deep the scan goes, defaulting to full depth
like the CLI `-R`.

## Archive input

Any page that reads a single image also accepts a `.zip`, `.7z`, `.rar`, `.tar`, or
`.tar.gz`/`.tgz` holding one. Pick or drop the archive and the app extracts the first member
matching the page's format to a temporary directory, runs it through the normal pipeline, and
deletes it when the job finishes. Output lands next to the archive, named after the member (so
`game.zip` holding `game.iso` produces `game.chd` beside the zip), and a matched `.cue` brings
its referenced bin tracks with it. This applies to a single file; folder scans still queue
plain files only, so unpack archives you want to batch from a tree. A single-file `.gz` with
no tar container is not supported.

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

## Comparison card

The compress-to-CHD, compress-to-CSO/ZSO, compress-to-RVZ (GameCube and Wii), compress-to-NSZ/XCZ,
CIA/CCI convert, CSO/ZSO-to-CHD, and CHD-to-CSO/ZSO pages show a comparison card above the output
log after each file finishes.
It lists the input and output size with the percent saved (or grown), the format transition (for
example ISO to RVZ), and, when the "Verify after conversion" toggle is on, the output's SHA-1 and
a verify badge. CHD, CSO/ZSO, and NSZ/XCZ re-decompress the whole output and note a round trip;
RVZ verifies structure without a full decode; CIA/CCI conversion has no integrity check to run, so
no badge is shown. If an NSZ/XCZ output can't actually be checked (for example the keyset has no
header key), the card reports it as not verified rather than showing a false pass.
Batch runs show one card per file. The toggle is off by default, since verification re-reads the
output and adds time proportional to its size.

## Option help

Every option control carries a small info glyph next to its label. Hovering or keyboard-focusing
it shows a tooltip explaining what the option does, with wording that matches the CLI help for
the same flag. The conflict picker additionally describes each policy inline in its dropdown.

The prod.keys row on Switch pages is colored green once the key file resolves and red when
none is found. It is re-probed live as files are added, so dropping keys into the default
location turns the row green without restarting the app.

## Option gating

The GUI disables options that do not apply to the current selection and explains why with a
tooltip. The Run button is disabled, with a tooltip giving the exact reason, whenever the
configuration cannot run, for example when no input is selected, when a queue is empty, or when
an output template field is set but blank.

## Completion notification and taskbar progress

When a batch finishes and the app window is not focused, the GUI fires a native OS
notification with a summary such as "42 of 43 done, 1 failed, 18.3 GiB saved". The title
switches to "Batch finished with errors" when any file failed. A "Completion sound" toggle
in the sidebar footer plays a short tone alongside the notification; it is off by default and
the notification itself always fires regardless of the toggle.

While a batch runs, the taskbar (Windows and Linux desktops that support it) or dock icon
(macOS) advances alongside the per-file progress bar, turns red on failure, and clears when
the batch finishes or is cancelled. Taskbar and dock progress are best-effort: platforms or
window managers without support are silently skipped, and the rest of the GUI is unaffected.

## Configuration

The GUI reads and writes the same `rom-converto.toml` presets the CLI does (see
[Configuration](configuration.md) for the file format, search order, and covered settings). A
preset saved in the GUI runs identically from the CLI with `--preset <name>`, and a preset
written by hand or by the CLI shows up in the GUI's preset picker.

The Settings page (Utilities section of the sidebar) lists every `[presets.<name>]` table found
in the config file, shows the resolved config path, and lets you delete a preset. Format pages
that expose config-covered options (GameCube, Wii, Switch, CHD, and CSO/ZSO compress, and Wii U
compress for `level`/`on_conflict`) show a preset control with:

- A picker to make a preset active. Making a preset active applies its values for that page's
  format into the page's own fields immediately, and again whenever you open a page with that
  preset still active, so the same profile follows you across pages without re-picking it.
- "Save current options as", which writes the page's current values into `[presets.<name>]`
  under that page's format key, creating the preset if the name is new or replacing it if it
  already exists.

Only the fields a page exposes as config-covered knobs are read or written: `--recursive`,
`--max-depth`, and the output template stay page-local, matching the CLI's own config
exclusions. `dat` presets are not writable from the GUI: the dat pages do not expose
`api_base`/checksum-tier controls, so there is nothing to bind them to.

Saving or deleting a preset only rewrites its own `[presets.<name>]` table; every other table,
key, and comment in the file is left as-is. The one caveat: comments placed *inside* a preset
table you edit (for example between two of its keys) are not preserved, since the GUI
regenerates that whole table from its current fields on every save.

A relative `output_dir` or `report` path in a preset is kept relative when the GUI saves it
back; the GUI does not resolve it against the config directory the way a running command does,
so a hand-authored relative path stays reproducible from the CLI on another machine.

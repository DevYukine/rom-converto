# Desktop GUI

The desktop app is a Tauri 2 application with a Nuxt 4, Vue, Pinia, and
Tailwind CSS frontend. It runs on Windows, macOS, and Linux. Its operations use
the same `rom-converto-lib` functions as the CLI.

## Use

Choose a format page, add files or folders, set its options, then run the queue.
Each page shows the equivalent CLI command with the selected options. The
Inspect page reads supported files through the same path as `rom-converto info`.

Batch pages keep pending and running work in Active, then move each item to
Completed or Failed. You can reorder pending work, select items to remove, retry
failed items, and choose 1 to 8 concurrent jobs. Wii U compression is one bundle
operation, so it does not expose those per-file queue controls.

Dropping a folder queues matching input files. Recursive scanning and its maximum
depth use the same library walker as the CLI. Pages with archive support accept
`.zip`, `.7z`, `.rar`, `.tar`, `.tar.gz`, and `.tgz` archives. The app extracts
the first matching member to a temporary directory and removes it after the job.

### CHD migration

Choose **CHD (old)** in Convert to migrate a v1 to v4 file to v5. The default output
is `<name>.v5.chd`. Version 5 and parent-dependent inputs are rejected. Hunk size
defaults to the source's; codecs default to its CD or general data layout.

The page supports folders, output templates, reports, and optional full verification
after conversion. It has no in-place mode. Inspect also reads legacy CHDs and shows
their version and stored hashes.

### Switch merge and split

Under Utilities, **Merge Switch NSP/XCI** stages all selected files into one job.
Choose NSP output (default), or XCI when every input is an XCI. The page supports
`prod.keys`, output directory and filename, conflicts, and preview. It displays a
persistent signature warning and is intended for emulator use.

The warning appears for both output formats. Selected NCA bytes stay unchanged;
generated XCI gamecard headers are unsigned.

**Split Switch NSP/XCI** takes one container and writes one NSP per title, defaulting
to `<stem>_split`. Both pages require unpacked NSP/XCI files and `prod.keys`; decompress
NSZ/XCZ first. Merge defaults to `<first stem> (Merged).nsp` or `.xci`.

### Xbox 360 GoD conversion

**Convert ISO to GoD** takes a disc ISO and writes `<stem>_god` by default. You can
choose an output directory and title, preview the job, or add folders to the queue.
Archive inputs are supported here even though the CLI conversion takes an ISO directly.

The page has no key, report, output-template, or verification setting. Unlike the CLI,
GUI conflict handling supports Rename; Overwrite if invalid skips existing output
because GoD has no integrity probe. Folder scanning creates separate queue jobs,
not a recursive `xenon convert` command.

## Output and safety controls

Switch split and GoD conversion overwrite multiple files directly. Use a separate
output directory if you need to retain the previous output after a failed or cancelled run.

Write pages provide `On conflict`: Overwrite, Skip, Rename, Error, and Overwrite
if invalid. Skip and Error leave an existing target unchanged. The last option
verifies supported outputs before choosing whether to retain or replace them.

Most write pages provide Preview. It uses the same planning logic as CLI
`--dry-run` and writes no conversion output. Archive previews may extract a file into a temporary directory, which is cleaned up afterward. The app also checks available space before a
write, using the input size plus 256 MiB as a conservative floor. A page option
can skip that check.

Cancel stops current work and removes its partial output. Completed files remain
completed. Batch completion can send an OS notification and update the taskbar or
dock where the desktop supports it.

## Shared configuration

The GUI reads and writes the CLI `rom-converto.toml` preset file. Presets for
GameCube, Wii, Switch, CHD, CSO/ZSO, and the covered Wii U compression options
can be selected and saved from their pages. The Settings page lists and deletes
presets. A GUI edit rewrites only the edited preset table, so comments within
that table are lost.

## Updates

The desktop build uses Tauri's signed updater. It checks five seconds after
launch and every four hours while automatic checks are enabled. The update toast
offers installation and restart, Later, or Skip this version. Later hides the
notice until the next launch; Skip persists for that release. Settings can
disable automatic checks or request a check immediately.

Installing is disabled while conversions are running because installation
relaunches the app. Update download progress and installation errors remain in
the toast. The GUI updater gets signed release metadata from the project's
`latest.json` release asset.

## CLI-only features

The GUI does not expose shell completions, `self-update`, terminal verbosity
flags, `--config`, `--preset`, `--no-update-check`, or `info --json`. It also
does not provide `dat identify`, `dat fixdat`, `dol migrate`, or `rvl migrate`
as dedicated pages. The relevant compression pages accept supported legacy inputs
and migrate them through their normal conversion flow.

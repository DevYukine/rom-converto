# Desktop GUI

The desktop app is a Tauri 2 application with a Nuxt 4, Vue, and Pinia
frontend. Styling is scoped component CSS plus design tokens; Tailwind is used
only for its preflight reset. It runs on Windows, macOS, and Linux. Its
operations use the same `rom-converto-lib` functions as the CLI.

## Use

Choose a format page, add files or folders, set its options, then run the queue.
Each page shows the equivalent CLI command with the selected options. The
Inspect page reads supported files through the same path as `rom-converto info`.

Batch pages keep running work in Running, pending work in Up next, and finished
or failed items in Finished this session. Failed items stay inside Finished
this session, with a Retry failed button to run them again. You can reorder
pending work, remove items with the per-row Remove button, and choose 1 to 8
concurrent jobs. Wii U compression is one bundle operation, so it does not
expose those per-file queue controls.

Dropping a folder queues matching input files. Recursive scanning and its maximum
depth use the same library walker as the CLI. Pages with archive support accept
`.zip`, `.7z`, `.rar`, `.tar`, `.tar.gz`, and `.tgz` archives, and a folder scan
queues an archive only when it holds a matching member, picked with the
extensions the page accepts, which match what the job later extracts with. Wii U
conversion scans both `.wud` and `.wux` whichever direction is set, so an archive
holding only the other kind is queued and then fails with "no matching image".
On pages that write output, an archive whose member also sits unpacked beside it
is left out in favor of the plain file; verify pages queue both.

The GUI does not compare the outputs of queued files. With On conflict set to
Overwrite, two files that name the same output (the same game as `.zip` and `.7z`,
or a `.cue` and an archive holding its `.iso`) replace each other, and so does a
file whose output is another queued input (converting `game.3ds` replaces a queued
`game.cia`). Use Skip, Rename, or Error for such folders. The CLI's `-R` skips them.

The app extracts the first matching member to a temporary directory and removes it
after the job. CHD verify with **Fix SHA-1** needs a plain `.chd`: an archive job
fails with that message, while the CLI `-R --fix` walks plain files only.

The Hash page hashes the file inside an archive that holds a known image, and the
archive's own bytes otherwise, like `rom-converto hash`. With Recursive on, dropping a
folder hashes every file in it.

### CHD migration

Choose **CHD (old)** in Convert to migrate a v1 to v4 file to v5. The default output
is `<name>.v5.chd`. Version 5 and parent-dependent inputs are rejected. Hunk size
defaults to the source's; codecs default to its CD or general data layout.

The page supports folders, output templates, reports, and optional full verification
after conversion. It has no in-place mode. Inspect also reads legacy CHDs and shows
their version and stored hashes.

### Switch merge and split

Under Tools, **Merge Switch NSP/XCI** stages all selected files into one job.
Choose NSP output (default), or XCI when every input is an XCI. The page supports
`prod.keys`, output directory and filename, conflicts, and a dry run. It displays a
persistent signature warning and is intended for emulator use.

The warning appears for both output formats. Selected NCA bytes stay unchanged;
generated XCI gamecard headers are unsigned.

**Split Switch NSP/XCI** takes one container and writes one NSP per title, defaulting
to `<stem>_split`. A single-title NSP is skipped; a single-title XCI converts to one
NSP. Both pages take plain NSP/XCI files or archives holding one, and need `prod.keys`;
decompress NSZ/XCZ first.
Merge defaults to `<first stem> (Merged).nsp` or `.xci`. Split does not offer Rename
because its output is a directory.

Under Decrypt, **Decrypt for NxEmu** converts an encrypted NSP or XCI into the DNSP or DXCI
that [NxEmu](https://www.nxemu.com/) loads. The page takes `prod.keys` and accepts plain
NSP/XCI files or archives holding one; decompress NSZ/XCZ first. The output is
`<name>.dnsp` or `<name>.dxci` and loads only in NxEmu.

### Bundle and unbundle 3DS ROMs

**Bundle 3DS ROM (Azahar)** stages every selected file into one job and packs them
into an Azahar bundle: one CCI/3DS or CXI main (or the Z3DS variants `.zcci`/`.zcxi`)
plus CIA/ZCIA titles such as updates and DLC, 50 members in total (49 CIAs plus a
main, or 50 in a `.bcia`). Members must be decrypted; encrypted members are refused. The bundle is written to the chosen directory, or next
to the main ROM as `<main ROM name>.bcci` (`.bcxi` with a CXI main, `.bcia` with
none) when no directory is set. A filename chosen in the File row must end in the
bundle extension the members produce.

**Unbundle 3DS ROM** extracts every member of a `.bcia`/`.bcci`/`.bcxi` file into a
directory, defaulting to `<stem>_unbundled` beside the input. It also reads a bundle
inside an archive. Both pages support
output directory, conflicts, and dry run. Unbundle does not offer Rename because
its output is a directory. It refuses a bundle with more than 50 members, and an
existing file at the output path needs Overwrite. Overwrite if invalid acts as Skip.

### Xbox 360 GoD conversion

**Convert ISO to GoD** takes a disc ISO, plain or inside an archive, and writes
`<stem>_god` by default. You can choose an output directory and title, start a dry
run, or add folders to the queue.

The page has no key, report, output-template, or verification setting. Directory-output
pages (Convert ISO to GoD, and the Xbox, Xbox 360, PSP, and PS Vita extract pages) do not
offer Rename because the output is a folder; Overwrite if invalid skips existing output
because GoD has no integrity probe. Folder scanning creates separate queue jobs,
not a recursive `xenon convert` command, so the title applies to every job while the
CLI rejects `--title` with `-R`.

### Organize a library

Under **Organize**, drop a library folder and pick a required output directory.
Every file is detected, optionally renamed through the online Playmatch
database, filed under `<output>/<console>/` following the layout template
(default `{console}/{basename}.{ext}`, tokens `{console} {title} {titleId}
{region} {serial} {basename} {ext} {language} {type} {dat} {game}
{input_dir}`, plus the frontend tokens `{adam} {batocera} {crossmix} {es}
{funkeyos} {minui} {mister} {miyoocfw} {onion} {pocket} {retrodeck}
{rocknix} {romm} {spruce} {twmenu}`), and converted to the best archival
format for its console: GameCube and Wii compress to RVZ, 3DS to Z3DS, Switch
to NSZ/XCZ (requires `prod.keys`), Wii U to WUA, PS1, PS2, Saturn, Sega CD,
Dreamcast and LaserDisc to CHD, PSP to CSO, Xbox to XISO, Xbox 360 to ZAR, PS3 to a
decrypted ISO, and cartridge ROMs including DS to ZIP. Files already in their target
format are copied unchanged; unrecognized files are skipped.

`Rename with DAT` hashes every file and queries the Playmatch API, including
during a dry run. `.m3u` playlists for multi-disc sets are written on real
runs only, never during a dry run. Move files deletes each source only after
its organized copy was written successfully.

The remaining options mirror the CLI flags (see [the CLI reference](cli.md#organize)).
The page shows the common ones first and groups the rest into collapsible
sections under the same headings as the CLI reference. A section starts open
when any of its options is set:

- **Choosing what gets organized**: excluded input globs, name-regex include
  and exclude, language and region filters, kinds to drop or keep, and
  retail-only.
- **One release per game**: keep one release per parent/clone group
  (needs the DAT match) and the preferences that pick the winner: name regex,
  verified, good, language, region, revision, retail, and parent. A separate
  file-name regex breaks ties between inputs for the same game or the same
  output path, after the already-placed and format preferences, whether or
  not One game per set is on.
- **Zip, links, and output layout**: letter folders with count, limit, and
  range grouping, folder per multi-disc game (with playlists on, the `.m3u`
  goes inside the game folder), TorrentZip or RVZSTD with a
  zip-exclusion glob, and hardlink, symlink, or reflink instead of copying
  already-correct files.
- **Headers, trimming, and patching**: header stripping (`nes`, `fds`, `a78`,
  `lnx`, `smc`/`sfc`), re-padding trimmed GBA/NDS dumps, and CRC32-matched
  patch application with a patch-only mode.
- **Cleaning up**: delete files in written output folders that the run did not
  produce (clean never deletes files under INPUT), with exclusion globs and a
  flat backup folder. Empty folders, shown with Move files, controls
  source-folder deletion after moves.

Verify after organize, under Safety, checks zips, copies, and conversions after
they are written. A conversion whose format has no output check (3DS, Wii U,
Xbox, Xbox 360, and PS3) fails as unverified and keeps its source.

## Output and safety controls

Switch split and GoD conversion overwrite multiple files directly. Use a separate
output directory if you need to retain the previous output after a failed or cancelled run.

Write pages provide `On conflict`: Overwrite, Skip, Rename, Error, and Overwrite
if invalid. Skip and Error leave an existing target unchanged. The last option
verifies supported outputs before choosing whether to retain or replace them.
Organize always starts at Error, regardless of the default policy setting.

Most write pages provide Dry run. It uses the same planning logic as CLI
`--dry-run`, writes no conversion output, and lists every planned row in a
scrollable modal. An archive dry run may extract a file into a temporary
directory, which is cleaned up afterward. The app also checks available space before a
write, using the input size plus 256 MiB as a conservative floor. A page option
can skip that check. Organize's Move files option deletes source files only
after their organized copies were written successfully; skipped or failed files keep
their sources, except with On conflict set to Overwrite if invalid: an existing
zip, copy, or hardlink (never a symlink) that verifies valid, meaning it holds this source's data,
counts as written and its source is deleted. Any item whose planned output path is its own source file
is left untouched as already in place, also through a symlinked folder, a bind mount, or a letter-case
variant of the path. Union filesystems such as mergerfs or Unraid's `/mnt/user` are not detected, so
use the same folder spelling for input and output there.

Cancel stops current work and removes its partial output. Completed files remain
completed. Batch completion can send an OS notification and update the taskbar or
dock where the desktop supports it.

## Shared configuration

The GUI reads and writes the CLI `rom-converto.toml` preset file. Presets for
GameCube, Wii, Switch, CHD, CSO/ZSO, and the covered Wii U compression options
can be selected and saved from their pages. The Settings page lists, edits, and
deletes presets. A GUI edit rewrites only the edited preset table, so comments within
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

The updater matches how the running copy was set up, so installed and
portable copies each receive their own package:

- Windows installer: downloads the signed setup exe and runs it.
- Windows portable `rom-converto-gui-windows-x64.exe`: downloads the signed
  portable exe, swaps it in place, and restarts, so its folder must be
  writable. The replaced exe is kept next to it with an `.old` suffix until
  the next launch removes it.
- Linux `.deb`: downloads the signed `.deb` and installs it with `dpkg`
  through a `pkexec` (or sudo) prompt.
- Linux AppImage: rewrites the running AppImage in place.
- macOS: replaces the `.app` bundle where it runs, whether it came from the
  `.dmg` or the `.app.zip`.

## CLI-only features

The GUI does not expose shell completions, `self-update`, terminal verbosity
flags, `--config`, `--preset`, `--no-update-check`, or `info --json`. It also
does not provide `dat identify`, `dat fixdat`, `dol migrate`, or `rvl migrate`
as dedicated pages. The relevant compression pages accept supported legacy inputs
and migrate them through their normal conversion flow. `nx split -R` (recursive
split with `--output-dir`) is also CLI-only: the Split page takes one container.

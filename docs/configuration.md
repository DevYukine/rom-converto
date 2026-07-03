# Configuration

A TOML config file lets you set per-format default flags and named presets so you do not have
to repeat long flag combinations. The config is optional: with no config file the built-in
defaults apply.

The GUI does not read this config file. Options are set per page in the app. The config file
applies to the CLI only.

## Search order

The first existing file in this order is used:

1. The path given to `--config <FILE>`. If you pass `--config` and the file does not exist,
   the command stops with an error.
2. `./rom-converto.toml` in the current directory.
3. `./.rom-converto.toml` in the current directory.
4. The per-user config directory:
   - Linux: `~/.config/rom-converto/config.toml`
   - macOS: `~/Library/Application Support/rom-converto/config.toml`
   - Windows: `%APPDATA%\rom-converto\config.toml`

## Precedence

For each setting, the value is resolved in this order, highest first:

1. An explicit flag on the command line.
2. The selected `--preset` value for that format.
3. The config file format default for that format.
4. The built-in default.

An unset flag never overrides a preset or config value. For example, if a preset sets
`level = 22` and you run a compress without `-l`, the level is 22; passing `-l 5` uses 5.

## Behavior

- A missing config file is not an error: the built-in defaults apply.
- A malformed config file is a hard error that names the file, so a typo is not silently
  ignored. Unknown keys are rejected the same way.
- An unknown `--preset` name stops with an error that lists the available preset names.
- Relative `output_dir` and `report` paths resolve against the directory that holds the
  config file, not the current directory.

## Covered settings

The config covers the tuning knobs worth repeating: `level`, `chunk_size`, `block_size_exp`,
`mode`, `hunk_size`, `block_size`, `on_conflict`, `output_dir`, `report`, and `api_base`,
each under the matching format table. The format tables are `[dol]`, `[rvl]`, `[nx]`,
`[chd]`, `[cso]`, `[wup]`, and `[dat]`.

Some flags are deliberately command-line only and are not read from the config: `--recursive`
and `--max-depth` (they change how much of a directory tree is processed),
`--output-template` (it changes which output file is written), the `cso` `--format`, and the
`chd` `--dvd`/`--cd`/`--zstd` selectors (they change which output container or codec set you
produce). Keeping these out of the config avoids silently changing what gets traversed or what
file is written.

## Example

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

[dat]
api_base = "https://playmatch.retrorealm.dev/api/v2"
report = "./dat-report.json"

[presets.archive]
dol = { level = 22, chunk_size = 131072 }
nx = { level = 22, mode = "solid" }

[presets.fast]
dol = { level = 5 }
nx = { level = 10 }
```

Run a preset with `rom-converto dol compress game.iso --preset archive`.

The `mode` key for `nx` is the NX codec mode (`solid` or `block`, the same as `nx --mode`); it
is unrelated to these named presets, which are bundles of settings you choose by name.

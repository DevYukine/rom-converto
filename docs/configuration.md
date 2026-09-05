# Configuration

`rom-converto.toml` is optional. It supplies defaults for repeated conversion
options and named presets. Without a config file, commands use built-in defaults.

## Finding the config file

The CLI uses the first existing path in this order:

1. `--config <FILE>`. This is the only path considered when the flag is present.
   A missing file is an error.
2. `./rom-converto.toml`
3. `./.rom-converto.toml`
4. The user config file:

| Platform | Path |
|---|---|
| Linux | `~/.config/rom-converto/config.toml` |
| macOS | `~/Library/Application Support/rom-converto/config.toml` |
| Windows | `%APPDATA%\rom-converto\config.toml` |

Malformed TOML and unknown keys are errors. Relative `output_dir` and `report` paths
resolve from the selected config file's directory. The GUI reads and writes presets in
the same file, while preserving relative paths when it saves them.
`ROM_CONVERTO_CONFIG` is not a supported environment variable. Use `--config`.

## Precedence

For each setting, the CLI uses:

1. An explicit command-line flag.
2. The selected `--preset` value.
3. The matching top-level table.
4. The command's built-in default.

Preset tables merge field by field over their matching top-level table. A preset that
sets only `dol.level`, for example, still inherits `dol.chunk_size` from `[dol]`.
An unknown preset is an error.

## Tables and keys

| Table | Accepted keys |
|---|---|
| `[dol]`, `[rvl]` | `level`, `chunk_size`, `on_conflict`, `output_dir`, `report` |
| `[nx]` | `level`, `mode`, `block_size_exp`, `on_conflict`, `output_dir`, `report` |
| `[chd]` | `hunk_size`, `codecs`, `level`, `on_conflict`, `output_dir`, `report` |
| `[cso]` | `block_size`, `on_conflict`, `output_dir`, `report` |
| `[wup]` | `level`, `on_conflict` |
| `[dat]` | `api_base`, `report`, `input_checksum_min`, `input_checksum_max` |

`[presets.NAME]` can contain any of these format tables. `on_conflict` accepts
`error`, `overwrite`, `skip`, `rename`, or `overwrite-invalid`.

`chunk_size` is a power of two from 32 KiB through 2 MiB. `nx.level` is 1 through
22, `nx.block_size_exp` is 14 through 32, and `wup.level` is 0 through 22. CHD
`codecs` is an array, for example `['cdlz', 'cdzl', 'cdfl']`. Command-line selectors
such as CHD `--cd`/`--dvd`/`--ld`, CSO `--format`, recursion, and output templates are
not config settings.

```toml
[dol]
level = 18
chunk_size = 131072
output_dir = "./rvz"
on_conflict = "skip"

[nx]
level = 18
mode = "solid"
block_size_exp = 20

[chd]
hunk_size = 4096
codecs = ["cdlz", "cdzl", "cdfl"]

[presets.fast.dol]
level = 5

[presets.archive.nx]
level = 22
mode = "solid"
```

Run a preset with `rom-converto dol compress game.iso --preset fast`.

NX merge and split use the configured `nx.output_dir`, including presets, but do not
inherit `nx.on_conflict`. Set their conflict policy on the command line.

## Key files

Key files are supplied separately from TOML configuration.

| Family | Resolution order |
|---|---|
| Switch `nx` | `--keys <FILE>`, then `~/.switch/prod.keys` on Linux/macOS or `%USERPROFILE%\.switch\prod.keys` on Windows, then `prod.keys` beside the executable |
| Wii U disc input | `--key <FILE>`, then sibling `<input>.key` or `game.key`, then the built-in database by filename, then a database probe |
| PS3 decrypt | `--key <FILE>`, then the built-in database by title ID, then sibling `<input>.dkey` |

Wii U key discovery applies to `.wud` and `.wux` inputs. `wup compress` may receive
multiple `--key` arguments; they are paired with disc inputs in command-line order.
A Wii U key is 16 raw bytes or 32 hexadecimal characters. A PS3 `.dkey` contains the
final 16-byte disc key as 32 hexadecimal characters; raw 16-byte `d1 .key` and IRD
files are not accepted.

3DS seed crypto also does not use TOML. `ctr decrypt` checks `seeddb.bin` in the
current working directory, then fetches a missing seed from Nintendo's CDN. `ctr info`
uses the local file only and reports whether a matching seed verifies.

The persistent hash and verify cache is separate from configuration. It is stored at
`rom-converto/hash-cache.json.gz` under the user config directory. Use
`--rebuild-cache` to recreate it.

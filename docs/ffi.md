# C ABI

`rom-converto-ffi` is the C ABI for hosts that embed rom-converto. Build it with:

```sh
cargo build --release -p rom-converto-ffi
```

The crate builds a `cdylib`: `rom_converto_ffi.dll` on Windows,
`librom_converto_ffi.so` on Linux and FreeBSD, and
`librom_converto_ffi.dylib` on macOS. Release archives include the library,
`include/rom_converto.h`, and `LICENSE`. Windows also includes
`rom_converto_ffi.dll.lib` for link time.

Include `rom_converto.h` and bind only its seven declared functions. ABI version
1 is reported by `rom_converto_version_json`; it also reports the library version,
the `rom-converto.run.v1` runner manifest, supported operations, and status codes.
Treat unknown JSON fields, status codes, and progress kinds as forward-compatible
extensions. ABI v1 is independent of the Rust package version and changes only
add optional data; existing declarations, required request fields, and meanings
remain the contract.

## Lifetime and calls

```c
RomConvertoContext *ctx = rom_converto_context_new();
char *response = NULL;
int32_t status = rom_converto_run_json(ctx, request_json, &response);
/* use response */
rom_converto_string_free(response);
rom_converto_context_free(ctx);
```

`request_json` is borrowed, NUL-terminated UTF-8. A non-null response is owned
by the caller and must be released exactly once with `rom_converto_string_free`.
The string returned by `rom_converto_version_json` follows the same rule. Both
free functions accept null.

Use one context for one active `rom_converto_run_json` call. A second concurrent
run returns `ROM_CONVERTO_INVALID_ARGUMENT`; reuse the context only after the
first call returns. `rom_converto_context_cancel` is safe from another thread and
requests cancellation without waiting. `rom_converto_context_free` cancels and
waits for an active run.

## Progress callbacks

Register a callback before running:

```c
rom_converto_context_set_progress(ctx, on_progress, user_data);
```

The callback receives borrowed UTF-8 event JSON that is valid only for that call.
Copy it if it must outlive the callback. Keep the callback and `user_data` valid
until replacing or clearing the registration returns, or until the context is
freed. The callback can run on an implementation thread. It must not unwind
across the C boundary or call `rom_converto_context_set_progress` or
`rom_converto_context_free` for the same context.

Progress events currently use `start`, `advance`, `phase`, `warn`, and `finish`
kinds. `advance` also carries a fractional total where available.

## JSON runner

Send a UTF-8 JSON request with an `operation`. Send
`"schema":"rom-converto.run.v1"` in production. The schema field is optional
for compatibility, but if supplied it must match. `op` and `command` are aliases
for `operation`.

```json
{
  "schema": "rom-converto.run.v1",
  "operation": "cso.compress",
  "input": "C:\\Games\\game.iso",
  "output": "C:\\Games\\game.cso",
  "options": { "on_conflict": "error" }
}
```

`rom_converto_version_json` is the source of truth for operation names and their
options: `runner_schema.operations` lists every name and alias, and
`runner_schema.common_options` the options shared across operations. Common
options include `on_conflict`, `recursive`, `output_dir`, `output_template`,
`max_depth`, `report`, `skip_space_check`, `verify_after`, `quick`,
`skip_probe`, `media_patch`, and `title`. `output` and
`options.output_template` cannot both be set. `dry_run: true` returns the plan
without writing files.

Request fields the runner reads from process state (a shared hash cache, a
frontend's default conflict policy) are not part of the JSON and cannot be
set through the C ABI.

### Operations

Operations added since the first ABI v1 release: `cue.to_iso`, `cue.to_cso`, `ntr.encrypt`, `ntr.decrypt` (the original `nds.encrypt` and `nds.decrypt` ids still resolve as aliases),
`nx.merge`, `nx.split`, `ps3.decrypt`, `psp.to_iso`, `psp.extract`,
`vita.extract`, `xbox.convert`, `xbox.extract`, `xenon.compress`,
`xenon.convert`, `xenon.extract`, and `xenon.verify`.

`nx.merge` takes its containers in `options.inputs` (the first names the
record) and its format in `options.format` (`nsp`, default, or `xci`).
`nx.split` writes into `output` or `options.output_dir`, defaulting to a
`<name>_split` directory next to the input. Directory-output operations
(`nx.split`, `psp.extract`, `vita.extract`, `xbox.extract`, `xenon.extract`,
`xenon.convert`, `wup.decrypt`) accept `on_conflict` `error`, `overwrite`,
and `skip` but not `rename`; `overwrite` replaces an existing file at the path
and writes into a non-empty directory as it is.

`xenon.convert` writes a Games on Demand container and returns `data` with
`title_id`, `media_id`, `part_count`, and `total_bytes`.

`chd.migrate` upgrades legacy CHDs to v5. It behaves like every other
conversion: `output` names a single file, otherwise the output is
`<name>.v5.chd` next to the input, re-rooted by `options.output_dir` or shaped
by `options.output_template`. Recursive runs, per-file conflict policies, and
reports all apply.

### Recursive runs

`options.recursive: true` walks `input` for the extensions the operation
handles (`ctr.cdn_to_cia` enumerates title directories instead) and processes
each file as its own child request. `options.output_dir` mirrors the source
tree under itself. When `input` is a single file, the request runs as a
single-file request instead of failing. Read-only operations (`hash` and the
`*.verify` operations) skip the free-space preflight.

Under `on_conflict: "error"`, a child whose output already exists is not a
failure: the run stays `ok` with status 0, and that file's record is
`skipped` with the refusal in its `error` field. Any other child error is a
`failed` record and the run ends with `partial_failure` (or `failed` when
nothing succeeded).

### Responses

Responses include `schema`, `ok`, numeric `status`, string `code`, `message`,
and optional `details`, `totals`, `records`, `events`, and operation-specific
`data`. Show `message` to users; retain `details` and record errors for
diagnostics.

`message` is short and stable in shape: `Wrote <path>` for a conversion,
`Skipped existing <path>` for a conflict skip, `Dry run planned.` for a
single-file dry run, and for a batch either `<n> files completed (<ok> ok,
<skipped> skipped).` or `<failed> of <n> files failed.`

Every file that ran, was skipped, or was planned produces a record in
`records`, and `totals` sums them. A record's `operation` is the short verb of
the operation name (`compress` for `cso.compress`, `to-chd` for `cso.to_chd`)
with ` (dry run)` appended under `dry_run`. Dry-run records carry the planned
`output_path`, `status` `skipped` (with the reason in `error`) when the plan
keeps an existing output, `ok` otherwise, and `output_bytes` 0. Single-file
requests produce records too, so `options.report` writes a report for them.

`data` depends on the operation:

- A dry run returns a plan line (`operation`, `input`, `output`, `decision`,
  `media`, `missing_keys`) for every operation that writes one output,
  `wup.decrypt`, `ctr.cdn_to_cia`, and `ctr.generate_cdn_ticket` included.
  Recursive dry runs return `{ "plans": [...] }`.
- A conversion returns `{ "comparison": { ... } }` with `input_bytes`,
  `output_bytes`, `ratio_pct`, `input_format`, and `output_format`. With
  `options.verify_after: true` it also carries `output_sha1` and a `verify`
  report (`ok`, `round_trip`, `message`); a cancel during that pass ends the
  run as `cancelled`.
- `hash` returns the digests; a recursive `hash` returns an array of
  `{ "path", "digests" }` rows.
- Verify, info, playlist, and `dat.*` operations return their own structures.

| Status | Code |
| ---: | --- |
| 0 | `ok` |
| 1 | `failed` |
| 2 | `invalid_argument` |
| 3 | `partial_failure` |
| 130 | `cancelled` |
| 255 | `internal_error` |

## CLI echo

`runner_schema.cli` maps every operation to its CLI spelling, for hosts that
show the equivalent command line: `ops` gives the subcommand path,
`flags` the flag for each option field (with `global` for flags parsed before
the subcommand and `kind` for how the value is passed), `op_flags` the option
fields each subcommand accepts, and `output` where each subcommand takes its
output path. Fields without a CLI equivalent are absent from `flags`.

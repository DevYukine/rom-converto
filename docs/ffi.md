# C ABI

`rom-converto-ffi` is the stable C ABI for hosts that can call C functions. Each
release asset extracts to a `rom-converto-ffi-<classifier>` directory containing
the platform library, `include/rom_converto.h`, and `LICENSE`. Unix assets are
`.tar.gz`; Windows assets are `.zip` and also contain
`rom_converto_ffi.dll.lib`, the link-time import library. Link against the import
library and load `rom_converto_ffi.dll` on Windows. Load
`librom_converto_ffi.so` on Linux and FreeBSD, or `librom_converto_ffi.dylib` on
macOS.

## ABI versioning

ABI v1 is the contract for the seven functions declared in
`include/rom_converto.h`, their C types, ownership, status values, and callback
behavior. Other DLL exports are implementation details and are unsupported. ABI v1
is independent of the Rust library's semantic version and of the JSON runner schema.
A release may change its library version without changing ABI v1. The current JSON
runner schema is `rom-converto.run.v1`.

Call `rom_converto_version_json` to obtain `abi_version`, `library_version`, the
runner schema manifest, supported operations, common options, and status codes.
Its returned string follows the ownership rule below.

Within ABI v1, changes are additive only: new optional JSON fields, response
fields, `data` fields, operations, progress kinds, and status codes may be added.
Existing header-declared functions, signatures, status meanings, required JSON
fields, and existing field meanings do not change. Hosts must ignore unknown JSON
object fields and unknown progress kinds, and must not require an exact field set.

## Functions and ownership

```c
RomConvertoContext* rom_converto_context_new(void);
void rom_converto_context_free(RomConvertoContext* ctx);
void rom_converto_context_cancel(RomConvertoContext* ctx);
void rom_converto_context_set_progress(
    RomConvertoContext* ctx,
    void (*callback)(const char* event_json, void* user_data),
    void* user_data
);
int32_t rom_converto_run_json(
    RomConvertoContext* ctx,
    const char* request_json,
    char** response_json_out
);
void rom_converto_string_free(char* ptr);
char* rom_converto_version_json(void);
```

- `rom_converto_context_new` returns an opaque context. Free it exactly once with
  `rom_converto_context_free`. Passing null to `rom_converto_context_free` is safe.
- `request_json` is a NUL-terminated UTF-8 string borrowed for the call.
- `response_json_out` receives an owned NUL-terminated UTF-8 string when non-null.
  Release it exactly once with `rom_converto_string_free`.
- `rom_converto_version_json` returns the same owned string type. Release it with
  `rom_converto_string_free`.
- Only free pointers returned by `rom_converto_run_json` or
  `rom_converto_version_json` with `rom_converto_string_free`. Passing null to
  `rom_converto_string_free` is safe.

## Context, callbacks, and cancellation

Use one context for one active `rom_converto_run_json` call. A concurrent second
run returns `invalid_argument`. Sequential runs are allowed after the prior call
returns.

Set the progress callback before starting a run. Replacing or clearing it (pass a
null callback) waits for an active run to finish. The callback receives a borrowed,
temporary UTF-8 JSON pointer. Copy it before returning. Keep the callback and
`user_data` valid until replacement returns or the context is freed. The callback
may run on an implementation thread, must not throw or unwind across the C
boundary, and must not call `rom_converto_context_set_progress` or
`rom_converto_context_free` on the same context.

`rom_converto_context_cancel` may be called from another thread while a run is
active. It requests cancellation and returns without waiting. `rom_converto_context_free`
cancels and waits for an active run, so it must not be called from that run's
callback. A cancellation request made before a run does not carry into a later run.

## Status codes

| Code | `code` | Meaning |
| ---: | --- | --- |
| `0` | `ok` | The requested operation completed. |
| `1` | `failed` | The operation failed. |
| `2` | `invalid_argument` | The context, JSON, schema, operation, or arguments were invalid. |
| `3` | `partial_failure` | A batch completed with both successful and failed records. |
| `130` | `cancelled` | Cancellation was observed. |
| `255` | `internal_error` | An internal failure occurred. |

`rom_converto_run_json` returns the same numeric status written to the response.
When a response is present, show `message` to users and reserve `details` and
`records[].error` for logs or an expandable error view.

## Request schema

Requests are typed UTF-8 JSON. `schema` is optional for compatibility, but
production hosts should send `"rom-converto.run.v1"`; a supplied schema must match
exactly. `operation` is required. `op` and `command` are accepted aliases for
it. Paths are strings in the host platform's normal path syntax. `options` rejects
unknown fields.

```json
{
  "schema": "rom-converto.run.v1",
  "operation": "cso.compress",
  "input": "C:\\Games\\game.iso",
  "output": "C:\\Games\\game.cso",
  "config": "C:\\Games\\rom-converto.toml",
  "preset": "archive",
  "dry_run": false,
  "options": {
    "on_conflict": "error",
    "recursive": false,
    "output_dir": "C:\\Games\\converted",
    "output_template": "{stem}.{ext}",
    "max_depth": 2,
    "report": "C:\\Games\\report.json"
  }
}
```

Common `options` are `on_conflict` (`error`, `overwrite`, `skip`, `rename`, or
`overwrite_invalid`; `overwrite-invalid` is an accepted alias), `recursive`,
`output_dir`, `output_template`, `max_depth`, and `report`. `output` and
`options.output_template` are mutually exclusive. Set `dry_run` to receive a plan
without changing files.

Operation-specific request shapes:

| Operations | Required shape | Relevant `options` |
| --- | --- | --- |
| `cso.*`, `chd.*`, `cso.to_chd`, `chd.to_cso`, `rvz.*`, `dol.*`, `rvl.*`, `ctr.decrypt`, `ctr.encrypt`, `ctr.compress`, `ctr.decompress`, `ctr.convert`, `nx.compress`, `nx.decompress`, `cue.merge` | `input`; `output` is optional unless the operation requires a destination. | Format-specific fields such as `format`, `mode`, `block_size`, `hunk_size`, `level`, `chunk_size`, `allow_zstd`, `skip_verify`, and `keys`. |
| `ctr.cdn_to_cia` | CDN directory `input`; optional `output`. | `cleanup`, `ensure_ticket_exists`, `decrypt`, `compress`, `output_dir`. |
| `ctr.generate_cdn_ticket` | CDN directory `input`; optional `output`. | None. |
| `wup.compress` | `input`, or `options.inputs` containing paths or `{ "path", "format", "key", "key_path" }` objects; optional `output`. | `inputs`, `level`. |
| `wup.decrypt`, `dat.fixdat` | Directory `input` and destination `output`. | `key` for `wup.decrypt`; `max_depth`, `api_base`, `dat_id`, `dat_name`, `platform`, `subset` for `dat.fixdat`. |
| `*.verify`, `hash`, `info` | `input`. | `full`, `deep`, `deep_verify`, `allow_encrypted`, `content_hashes`, `key`, or `algo` as applicable. |
| `playlist.write` | Directory `input`. | `extensions`, `playlist_mode`, `output_dir`, `max_depth`. |
| `dat.verify`, `dat.identify` | File `input`. | `algo`, `api_base`, `report`, `input_checksum_min`, `input_checksum_max`. |
| `dat.scan` | Directory `input`. | `algo`, `api_base`, `max_depth`, `report`. |
| `dat.rename` | File or directory `input`. | `algo`, `api_base`, `max_depth`, `on_conflict`. |

The version manifest is authoritative for operation names. For `dat.verify` and
`dat.identify`, checksum bounds accept `crc32`, `md5`, `sha1`, or `sha256`;
the defaults are `crc32` and `sha256`, and the minimum cannot be stronger than
the maximum. `options.algo` cannot request a digest stronger than
`input_checksum_max`. Where an operation accepts a single file input, it also
accepts a `.zip`, `.7z`, `.rar`, `.tar`, `.tar.gz`, or `.tgz` archive using
the first matching member. Reject a request when the operation requires a shape not
supplied above rather than inferring a path.

## Response and progress schemas

Every response has this envelope. Fields marked optional are omitted when not
applicable.

```json
{
  "schema": "rom-converto.run.v1",
  "ok": true,
  "status": 0,
  "code": "ok",
  "message": "...",
  "details": "...",
  "totals": {
    "total_files": 1,
    "ok": 1,
    "skipped": 0,
    "failed": 0,
    "total_input_bytes": 0,
    "total_output_bytes": 0,
    "elapsed_ms": 0
  },
  "records": [{
    "input_path": "...",
    "output_path": "...",
    "operation": "...",
    "status": "ok",
    "input_bytes": 0,
    "output_bytes": 0,
    "ratio_pct": 0,
    "elapsed_ms": 0,
    "error": null
  }],
  "data": {}
}
```

`totals` and `records` describe file or batch work. `data` is the
operation-specific result:

| Operations | `data` shape |
| --- | --- |
| File conversion and `dry_run` | A plan with `operation`, `input`, `output`, decision fields, and optional media or missing-key detail. Recursive dry runs return `{ "plans": [...] }`. |
| File conversion after writing | `{ "comparison": { "input_bytes", "output_bytes", "ratio_pct", "input_format", "output_format" } }` when a comparison applies. |
| `hash` | `{ "crc32", "sha1", "md5", "sha256", "size_bytes" }`; unrequested digests are null. |
| `*.verify`, `info` | A format-specific verification or inspection object. |
| `playlist.write` | `{ "playlists": [{ "base_title", "output", "contents", "disc_count", "has_duplicate_numbers" }] }`. |
| `dat.verify`, `dat.identify` | A match object with `kind`, `path`, `verdict`, match metadata, and optional `error`. |
| `dat.scan` | `{ "rows": [match, ...] }`. |
| `dat.rename` | `{ "rows": [{ "from", "to", "action", "detail" }], "dry_run": bool }`. |
| `dat.fixdat` | `{ "dat_file": ..., "missing_count": number }`. |

Progress is delivered only to the registered callback as one JSON object per
invocation. Its tagged shapes are:

```json
{ "kind": "start", "total": 42, "message": "..." }
{ "kind": "advance", "delta": 1 }
{ "kind": "phase", "message": "..." }
{ "kind": "warn", "message": "..." }
{ "kind": "finish" }
```

There is no first-party C# wrapper or smoke test yet. Hosts should bind directly
to `include/rom_converto.h` and apply this contract.

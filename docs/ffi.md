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
options. Common options include `on_conflict`, `recursive`, `output_dir`,
`output_template`, `max_depth`, and `report`. `output` and
`options.output_template` cannot both be set. `dry_run: true` returns the plan
without writing files.

The `chd.migrate` operation upgrades legacy CHDs to v5. It supports per-file conflict
policies and reports, but ignores `options.output_dir` and `options.output_template`.
Set top-level `output` for a single file; recursive runs write sibling `.v5.chd` files.
It has no `in_place` option. The schema remains `rom-converto.run.v1`.

Switch merge/split and Xbox 360 GoD conversion are available through the CLI and GUI,
but have no JSON runner operation or C ABI entry point.

Responses include `schema`, `ok`, numeric `status`, string `code`, `message`,
and optional `details`, `totals`, `records`, and operation-specific `data`. Show
`message` to users; retain `details` and record errors for diagnostics.

| Status | Code |
| ---: | --- |
| 0 | `ok` |
| 1 | `failed` |
| 2 | `invalid_argument` |
| 3 | `partial_failure` |
| 130 | `cancelled` |
| 255 | `internal_error` |

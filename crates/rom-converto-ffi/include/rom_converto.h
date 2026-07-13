#ifndef ROM_CONVERTO_H
#define ROM_CONVERTO_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct RomConvertoContext RomConvertoContext;
typedef void (*RomConvertoProgressCallback)(const char *event_json, void *user_data);

enum RomConvertoStatus {
    ROM_CONVERTO_OK = 0,
    ROM_CONVERTO_FAILED = 1,
    ROM_CONVERTO_INVALID_ARGUMENT = 2,
    ROM_CONVERTO_PARTIAL_FAILURE = 3,
    ROM_CONVERTO_CANCELLED = 130,
    ROM_CONVERTO_INTERNAL_ERROR = 255,
};

RomConvertoContext *rom_converto_context_new(void);

/* Cancels and waits for an active run. Do not call from a progress callback. */
void rom_converto_context_free(RomConvertoContext *ctx);

void rom_converto_context_cancel(RomConvertoContext *ctx);

/*
 * Waits for an active run before replacing or clearing the callback. After
 * this returns, the old user_data is no longer callable. Do not call from a
 * progress callback. Pass NULL to clear the registration.
 * event_json is valid only for the duration of the callback.
 */
void rom_converto_context_set_progress(
    RomConvertoContext *ctx,
    RomConvertoProgressCallback callback,
    void *user_data);

/*
 * Runs one request synchronously. A second run on the same context returns
 * ROM_CONVERTO_INVALID_ARGUMENT. response_json_out may be NULL. Otherwise the
 * returned string must be released with rom_converto_string_free.
 */
int32_t rom_converto_run_json(
    RomConvertoContext *ctx,
    const char *request_json,
    char **response_json_out);

void rom_converto_string_free(char *ptr);

/* Returns the ABI and runner schema manifest as an owned JSON string. */
char *rom_converto_version_json(void);

#ifdef __cplusplus
}
#endif

#endif

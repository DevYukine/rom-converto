use rom_converto_lib::runner::models::{FfiVersionManifest, ProgressEvent, RunResponse, RunStatus};
use rom_converto_lib::runner::run_json_with_progress;
use rom_converto_lib::util::{CancelToken, ProgressReporter};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

const ABI_SCHEMA: &str = "rom-converto.ffi.v1";
const ABI_VERSION: u32 = 1;

static CONTEXTS: OnceLock<Mutex<HashMap<usize, Arc<RomConvertoContext>>>> = OnceLock::new();

type ProgressCallback = Option<unsafe extern "C" fn(*const c_char, *mut c_void)>;

#[repr(C)]
pub struct RomConvertoContext {
    runtime: tokio::runtime::Runtime,
    state: Mutex<ContextState>,
    idle: Condvar,
}

struct ContextState {
    active: bool,
    closing: bool,
    cancel: Option<CancelToken>,
    progress: ProgressCallbackState,
}

#[derive(Clone, Copy)]
struct ProgressCallbackState {
    callback: ProgressCallback,
    user_data: usize,
}

struct FfiProgress {
    callback: ProgressCallbackState,
}

struct ActiveRun<'a> {
    context: &'a RomConvertoContext,
    cancel: CancelToken,
    progress: FfiProgress,
}

impl RomConvertoContext {
    fn lock_state(&self) -> MutexGuard<'_, ContextState> {
        self.state.lock().unwrap_or_else(|err| err.into_inner())
    }

    fn start_run(&self) -> Result<ActiveRun<'_>, &'static str> {
        let mut state = self.lock_state();
        if state.closing {
            return Err("rom-converto context is closing.");
        }
        if state.active {
            return Err("A run is already active on this context.");
        }

        let cancel = CancelToken::new();
        let progress = FfiProgress {
            callback: state.progress,
        };
        state.active = true;
        state.cancel = Some(cancel.clone());
        drop(state);

        Ok(ActiveRun {
            context: self,
            cancel,
            progress,
        })
    }

    fn cancel(&self) {
        let state = self.lock_state();
        if !state.closing
            && let Some(cancel) = &state.cancel
        {
            cancel.cancel();
        }
    }

    fn set_progress(&self, progress: ProgressCallbackState) {
        let mut state = self.lock_state();
        while state.active {
            state = self.idle.wait(state).unwrap_or_else(|err| err.into_inner());
        }
        if !state.closing {
            state.progress = progress;
        }
    }

    fn close(&self) {
        let mut state = self.lock_state();
        state.closing = true;
        if let Some(cancel) = &state.cancel {
            cancel.cancel();
        }
        self.idle.notify_all();
    }

    fn wait_until_idle(&self) {
        let mut state = self.lock_state();
        while state.active {
            state = self.idle.wait(state).unwrap_or_else(|err| err.into_inner());
        }
    }
}

impl Drop for ActiveRun<'_> {
    fn drop(&mut self) {
        let mut state = self.context.lock_state();
        state.active = false;
        state.cancel = None;
        self.context.idle.notify_all();
    }
}

fn contexts() -> &'static Mutex<HashMap<usize, Arc<RomConvertoContext>>> {
    CONTEXTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lookup_context(ctx: *mut RomConvertoContext) -> Option<Arc<RomConvertoContext>> {
    if ctx.is_null() {
        return None;
    }
    contexts()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .get(&(ctx as usize))
        .cloned()
}

fn remove_context(ctx: *mut RomConvertoContext) -> Option<Arc<RomConvertoContext>> {
    if ctx.is_null() {
        return None;
    }
    let mut contexts = contexts().lock().unwrap_or_else(|err| err.into_inner());
    let ctx = contexts.remove(&(ctx as usize));
    if let Some(ctx) = &ctx {
        ctx.close();
    }
    ctx
}

impl FfiProgress {
    fn emit(&self, event: ProgressEvent) {
        let Some(callback) = self.callback.callback else {
            return;
        };
        let Ok(json) = serde_json::to_string(&event) else {
            return;
        };
        let Ok(c_json) = CString::new(json) else {
            return;
        };
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            callback(c_json.as_ptr(), self.callback.user_data as *mut c_void);
        }));
    }
}

impl ProgressReporter for FfiProgress {
    fn start(&self, total: u64, msg: &str) {
        self.emit(ProgressEvent::Start {
            total,
            message: msg.to_string(),
        });
    }

    fn inc(&self, delta: u64) {
        self.emit(ProgressEvent::Advance { delta });
    }

    fn finish(&self) {
        self.emit(ProgressEvent::Finish);
    }

    fn set_phase(&self, label: &str) {
        self.emit(ProgressEvent::Phase {
            message: label.to_string(),
        });
    }

    fn warn(&self, message: &str) {
        self.emit(ProgressEvent::Warn {
            message: message.to_string(),
        });
    }
}

#[unsafe(no_mangle)]
/// # Safety
///
/// The returned pointer must be freed exactly once with
/// `rom_converto_context_free`.
pub unsafe extern "C" fn rom_converto_context_new() -> *mut RomConvertoContext {
    let result = catch_unwind(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map(|runtime| {
                let ctx = Arc::new(RomConvertoContext {
                    runtime,
                    state: Mutex::new(ContextState {
                        active: false,
                        closing: false,
                        cancel: None,
                        progress: ProgressCallbackState {
                            callback: None,
                            user_data: 0,
                        },
                    }),
                    idle: Condvar::new(),
                });
                let ptr = Arc::as_ptr(&ctx).cast_mut();
                contexts()
                    .lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .insert(ptr as usize, ctx);
                ptr
            })
    });
    match result {
        Ok(Ok(ptr)) => ptr,
        _ => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
/// # Safety
///
/// `ctx` must be null or a pointer returned by `rom_converto_context_new` that
/// has not already been freed. This cancels and waits for an active run. It
/// must not be called from that run's progress callback.
pub unsafe extern "C" fn rom_converto_context_free(ctx: *mut RomConvertoContext) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(ctx) = remove_context(ctx) else {
            return;
        };
        ctx.wait_until_idle();
    }));
}

#[unsafe(no_mangle)]
/// # Safety
///
/// `ctx` must be null or a live pointer returned by `rom_converto_context_new`.
pub unsafe extern "C" fn rom_converto_context_cancel(ctx: *mut RomConvertoContext) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(ctx) = lookup_context(ctx) else {
            return;
        };
        ctx.cancel();
    }));
}

#[unsafe(no_mangle)]
/// # Safety
///
/// `ctx` must be null or live. This waits for an active run before replacing
/// the registration, so it must not be called from that run's callback.
/// `callback` must not unwind across the FFI boundary, and `user_data` must
/// remain valid until this function replaces the registration or the context
/// is freed.
pub unsafe extern "C" fn rom_converto_context_set_progress(
    ctx: *mut RomConvertoContext,
    callback: ProgressCallback,
    user_data: *mut c_void,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(ctx) = lookup_context(ctx) else {
            return;
        };
        ctx.set_progress(ProgressCallbackState {
            callback,
            user_data: user_data as usize,
        });
    }));
}

#[unsafe(no_mangle)]
/// # Safety
///
/// `ctx` must be live. `request_json` must point to a valid NUL-terminated
/// UTF-8 string. `response_json_out`, when non-null, must be writable.
pub unsafe extern "C" fn rom_converto_run_json(
    ctx: *mut RomConvertoContext,
    request_json: *const c_char,
    response_json_out: *mut *mut c_char,
) -> i32 {
    if !response_json_out.is_null() {
        unsafe {
            *response_json_out = ptr::null_mut();
        }
    }
    let Some(ctx) = lookup_context(ctx) else {
        return write_response(
            response_json_out,
            RunResponse::error(
                RunStatus::InvalidArgument,
                "rom-converto context is null.",
                None,
            ),
        );
    };
    let run = match ctx.start_run() {
        Ok(run) => run,
        Err(message) => {
            return write_response(
                response_json_out,
                RunResponse::error(RunStatus::InvalidArgument, message, None),
            );
        }
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        if request_json.is_null() {
            return write_response(
                response_json_out,
                RunResponse::error(RunStatus::InvalidArgument, "Request JSON is null.", None),
            );
        }
        let request = match unsafe { CStr::from_ptr(request_json) }.to_str() {
            Ok(s) => s.to_owned(),
            Err(err) => {
                return write_response(
                    response_json_out,
                    RunResponse::error(
                        RunStatus::InvalidArgument,
                        "Request JSON is not valid UTF-8.",
                        Some(err.to_string()),
                    ),
                );
            }
        };
        let response = ctx.runtime.block_on(run_json_with_progress(
            &request,
            &run.progress,
            run.cancel.clone(),
        ));
        write_response(response_json_out, response)
    }));

    let status = match result {
        Ok(status) => status,
        Err(payload) => write_response(
            response_json_out,
            RunResponse::error(
                RunStatus::InternalError,
                "rom-converto hit an internal error.",
                panic_details(payload),
            ),
        ),
    };
    drop(run);
    status
}

#[unsafe(no_mangle)]
/// # Safety
///
/// `ptr` must be null or a pointer returned by this library from
/// `rom_converto_run_json` or `rom_converto_version_json`.
pub unsafe extern "C" fn rom_converto_string_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        drop(CString::from_raw(ptr));
    }
}

#[unsafe(no_mangle)]
/// # Safety
///
/// The returned pointer must be freed with `rom_converto_string_free`.
pub unsafe extern "C" fn rom_converto_version_json() -> *mut c_char {
    catch_unwind(|| {
        let manifest =
            FfiVersionManifest::current(ABI_SCHEMA, ABI_VERSION, env!("CARGO_PKG_VERSION"));
        let json = serde_json::to_string(&manifest)
            .unwrap_or_else(|_| r#"{"status":255,"code":"internal_error"}"#.to_string());
        string_to_raw(json)
    })
    .unwrap_or_else(|_| string_to_raw(r#"{"status":255,"code":"internal_error"}"#.to_string()))
}

fn write_response(out: *mut *mut c_char, response: RunResponse) -> i32 {
    if out.is_null() {
        return response.status;
    }

    let (status, json) = match serde_json::to_string(&response) {
        Ok(json) => (response.status, json),
        Err(_) => (
            RunStatus::InternalError.as_i32(),
            r#"{"schema":"rom-converto.run.v1","ok":false,"status":255,"code":"internal_error","message":"Failed to serialize response."}"#.to_string(),
        ),
    };
    unsafe {
        *out = string_to_raw(json);
    }
    status
}

fn string_to_raw(s: String) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| CString::new("string contained interior nul").unwrap())
        .into_raw()
}

fn panic_details(payload: Box<dyn std::any::Any + Send>) -> Option<String> {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    fn context() -> Arc<RomConvertoContext> {
        Arc::new(RomConvertoContext {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap(),
            state: Mutex::new(ContextState {
                active: false,
                closing: false,
                cancel: None,
                progress: ProgressCallbackState {
                    callback: None,
                    user_data: 0,
                },
            }),
            idle: Condvar::new(),
        })
    }

    fn take_string(ptr: *mut c_char) -> String {
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe {
            rom_converto_string_free(ptr);
        }
        s
    }

    #[test]
    fn version_json_returns_owned_string() {
        let ptr = unsafe { rom_converto_version_json() };
        let value: serde_json::Value = serde_json::from_str(&take_string(ptr)).unwrap();
        assert_eq!(value["schema"], ABI_SCHEMA);
        assert_eq!(value["abi_version"], ABI_VERSION);
        assert_eq!(value["runner_schema"]["schema"], "rom-converto.run.v1");
        assert_eq!(value["status_codes"]["internal_error"], 255);
    }

    #[test]
    fn only_one_run_is_active_and_each_run_gets_a_fresh_token() {
        let ctx = context();
        let first = ctx.start_run().unwrap();
        assert!(ctx.start_run().is_err());

        first.cancel.cancel();
        drop(first);
        let second = ctx.start_run().unwrap();
        assert!(!second.cancel.is_cancelled());
    }

    #[test]
    fn progress_replacement_waits_until_idle() {
        let ctx = context();
        ctx.set_progress(ProgressCallbackState {
            callback: None,
            user_data: 1,
        });
        let run = ctx.start_run().unwrap();
        assert_eq!(run.progress.callback.user_data, 1);

        let setter_ctx = Arc::clone(&ctx);
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let setter = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            setter_ctx.set_progress(ProgressCallbackState {
                callback: None,
                user_data: 2,
            });
            done_tx.send(()).unwrap();
        });

        started_rx.recv().unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
        drop(run);
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        setter.join().unwrap();
        assert_eq!(ctx.lock_state().progress.user_data, 2);
    }

    #[test]
    fn context_free_cancels_and_waits_for_active_run() {
        let ctx = unsafe { rom_converto_context_new() };
        assert!(!ctx.is_null());
        let ctx_owner = lookup_context(ctx).unwrap();
        let run = ctx_owner.start_run().unwrap();
        let cancel = run.cancel.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let ctx_address = ctx as usize;
        let free = std::thread::spawn(move || {
            unsafe { rom_converto_context_free(ctx_address as *mut RomConvertoContext) };
            done_tx.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while !cancel.is_cancelled() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(cancel.is_cancelled());
        assert!(done_rx.recv_timeout(Duration::from_millis(50)).is_err());
        drop(run);
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        free.join().unwrap();
    }

    #[test]
    fn closing_context_rejects_late_runs_and_waits_for_callback_users() {
        let ctx = unsafe { rom_converto_context_new() };
        let ctx_owner = lookup_context(ctx).unwrap();
        ctx_owner.set_progress(ProgressCallbackState {
            callback: None,
            user_data: 1,
        });
        let run = ctx_owner.start_run().unwrap();

        let ctx_address = ctx as usize;
        let (free_tx, free_rx) = mpsc::channel();
        let free = std::thread::spawn(move || {
            unsafe { rom_converto_context_free(ctx_address as *mut RomConvertoContext) };
            free_tx.send(()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !ctx_owner.lock_state().closing && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(ctx_owner.lock_state().closing);
        assert!(lookup_context(ctx).is_none());
        assert!(ctx_owner.start_run().is_err());

        let setter_ctx = Arc::clone(&ctx_owner);
        let (setter_started_tx, setter_started_rx) = mpsc::channel();
        let (setter_tx, setter_rx) = mpsc::channel();
        let setter = std::thread::spawn(move || {
            setter_started_tx.send(()).unwrap();
            setter_ctx.set_progress(ProgressCallbackState {
                callback: None,
                user_data: 2,
            });
            setter_tx.send(()).unwrap();
        });
        setter_started_rx.recv().unwrap();
        assert!(setter_rx.recv_timeout(Duration::from_millis(50)).is_err());
        assert!(free_rx.recv_timeout(Duration::from_millis(50)).is_err());

        drop(run);
        setter_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        free_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        setter.join().unwrap();
        free.join().unwrap();
        assert_eq!(ctx_owner.lock_state().progress.user_data, 1);
    }

    #[test]
    fn panic_drops_active_run_state() {
        let ctx = context();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _run = ctx.start_run().unwrap();
            panic!("test panic");
        }));
        assert!(result.is_err());
        assert!(ctx.start_run().is_ok());
    }

    #[test]
    fn null_context_returns_invalid_argument() {
        let request = CString::new(r#"{"operation":"hash"}"#).unwrap();
        let mut out = ptr::null_mut();
        let status = unsafe { rom_converto_run_json(ptr::null_mut(), request.as_ptr(), &mut out) };
        assert_eq!(status, 2);
        let text = take_string(out);
        assert!(text.contains("context is null"));
    }

    #[test]
    fn invalid_json_returns_invalid_argument() {
        let ctx = unsafe { rom_converto_context_new() };
        assert!(!ctx.is_null());
        let request = CString::new("{").unwrap();
        let mut out = ptr::null_mut();
        let status = unsafe { rom_converto_run_json(ctx, request.as_ptr(), &mut out) };
        assert_eq!(status, 2);
        let text = take_string(out);
        assert!(text.contains("Request JSON is invalid"));
        unsafe {
            rom_converto_context_free(ctx);
        }
    }
}

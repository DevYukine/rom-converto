//! Update check and install. An installed build hands the downloaded
//! package to the updater plugin (NSIS installer, macOS app bundle, AppImage).
//! A portable Windows exe has nothing to install into, so it swaps the
//! running executable in place and restarts.

use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::ipc::Channel;
use tauri::utils::platform::bundle_type;
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::commands::ActiveCancel;
use crate::err_to_string;

const PORTABLE_SUFFIX: &str = "-portable";

/// The update found by the last `cmd_update_check`, consumed by `cmd_update_install`.
#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<Update>>);

#[derive(serde::Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "updater.ts"))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateEvent {
    /// Sent once per whole percent while the package downloads.
    Progress { downloaded: u64, total: u64 },
    /// Download verified; the package is being installed.
    Installing,
}

/// Collapses per-chunk download callbacks into one event per whole percent,
/// so a large package does not flood the IPC channel.
#[derive(Default)]
struct PercentThrottle {
    downloaded: u64,
    percent: Option<u64>,
}

impl PercentThrottle {
    fn advance(&mut self, chunk: usize, total: Option<u64>) -> Option<UpdateEvent> {
        self.downloaded += chunk as u64;
        let total = total.filter(|t| *t > 0)?;
        let next = self.downloaded * 100 / total;
        if self.percent == Some(next) {
            return None;
        }
        self.percent = Some(next);
        Some(UpdateEvent::Progress {
            downloaded: self.downloaded,
            total,
        })
    }
}

/// The NSIS bundler stamps the exe it packs into the installer, so an
/// unstamped exe is the standalone download. An exe copied out of its install
/// directory has no `uninstall.exe` sibling and is treated as portable too:
/// swapping it in place is the only update that lands where it runs.
fn is_portable() -> bool {
    cfg!(windows)
        && (bundle_type().is_none()
            || std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| !dir.join("uninstall.exe").exists()))
                .unwrap_or(false))
}

#[tauri::command]
pub async fn cmd_update_check(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<Option<String>, String> {
    let mut builder = app.updater_builder();
    if is_portable() {
        let target = tauri_plugin_updater::target().ok_or("unsupported platform")?;
        builder = builder.target(format!("{target}{PORTABLE_SUFFIX}"));
    }
    let update = builder
        .build()
        .map_err(err_to_string)?
        .check()
        .await
        .map_err(err_to_string)?;
    let version = update.as_ref().map(|u| u.version.clone());
    *pending.0.lock() = update;
    Ok(version)
}

#[tauri::command]
pub async fn cmd_update_install(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
    cancel: State<'_, ActiveCancel>,
    on_event: Channel<UpdateEvent>,
) -> Result<(), String> {
    // Installing restarts or replaces the process, which would cut short
    // any conversion still running; holding the lock keeps new jobs from
    // starting until the install has failed or the process restarted.
    let active = cancel.lock().await;
    if !active.is_empty() {
        return Err("wait for running jobs to finish before installing the update".to_string());
    }
    let update = pending.0.lock().take().ok_or("no pending update")?;

    let mut throttle = PercentThrottle::default();
    let bytes = update
        .download(
            |chunk, total| {
                if let Some(event) = throttle.advance(chunk, total) {
                    let _ = on_event.send(event);
                }
            },
            || {},
        )
        .await
        .map_err(err_to_string)?;
    let _ = on_event.send(UpdateEvent::Installing);

    if update.target.ends_with(PORTABLE_SUFFIX) {
        tokio::task::spawn_blocking(move || swap_portable(&bytes))
            .await
            .map_err(err_to_string)??;
    } else {
        // On Windows the plugin launches the installer and exits the process
        // itself; macOS and Linux replace the bundle in place and return.
        // Like the portable swap, the install blocks, so keep it off the
        // async runtime.
        tokio::task::spawn_blocking(move || update.install(bytes))
            .await
            .map_err(err_to_string)?
            .map_err(err_to_string)?;
    }
    app.request_restart();
    Ok(())
}

fn swap_portable(bytes: &[u8]) -> Result<(), String> {
    // These bytes replace the running exe as is, so anything that is not
    // a PE image would trade a working binary for a dead one.
    if !bytes.starts_with(b"MZ") {
        return Err("portable update is not a Windows executable".to_string());
    }
    let exe = std::env::current_exe().map_err(err_to_string)?;
    let staged = sibling(&exe, ".new");
    let swapped = swap_staged(&exe, &staged, bytes);
    if swapped.is_err() {
        // Whatever failed, the running binary is in place and the staged
        // file is dead weight; nothing at launch cleans a stale .new.
        let _ = std::fs::remove_file(&staged);
    }
    swapped
}

fn swap_staged(exe: &Path, staged: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = exe
        .parent()
        .ok_or("current executable path has no parent directory")?;
    let old = sibling(exe, ".old");

    // Staged next to the exe so the swap is a same-volume rename.
    let mut file = std::fs::File::create(staged).map_err(|e| {
        format!(
            "cannot write to {} ({e}); the folder must be writable to update in place",
            dir.display()
        )
    })?;
    std::io::Write::write_all(&mut file, bytes).map_err(err_to_string)?;
    // Nothing orders the data before the renames below; a crash could
    // otherwise leave an empty binary in place.
    file.sync_all().map_err(err_to_string)?;
    drop(file);

    // A running exe can be renamed but not overwritten.
    std::fs::rename(exe, &old).map_err(err_to_string)?;
    if let Err(e) = rename_with_retry(staged, exe) {
        // Put the running binary back so the user is never left without one.
        if let Err(rollback) = std::fs::rename(&old, exe) {
            return Err(format!(
                "failed to install the new binary ({e}) and could not restore the old one ({rollback}); rename {} back to {} by hand",
                old.display(),
                exe.display()
            ));
        }
        return Err(e.to_string());
    }
    Ok(())
}

/// Antivirus and indexers briefly hold a freshly written exe open on Windows,
/// so the final rename is retried before giving up.
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(e) if attempt < 5 => {
                log::debug!(
                    "rename {} -> {} failed ({e}), retrying",
                    from.display(),
                    to.display()
                );
                attempt += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
            result => return result,
        }
    }
}

/// Removes the executable a portable update replaced. The previous process
/// may still be exiting when the new one starts, so the delete is retried
/// off the main thread; a leftover is picked up on the next launch.
pub fn cleanup_old_executable() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let old = sibling(&exe, ".old");
    if !old.exists() {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..10 {
            if std::fs::remove_file(&old).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    exe.with_file_name(name)
}

#[cfg(test)]
mod throttle_tests {
    use super::{PercentThrottle, UpdateEvent};

    fn progress(event: Option<UpdateEvent>) -> Option<u64> {
        match event? {
            UpdateEvent::Progress { downloaded, .. } => Some(downloaded),
            UpdateEvent::Installing => unreachable!(),
        }
    }

    #[test]
    fn emits_once_per_whole_percent() {
        let mut throttle = PercentThrottle::default();
        // The first chunk reports even at 0%, so the bar leaves the unknown state.
        assert_eq!(progress(throttle.advance(1, Some(400))), Some(1));
        assert_eq!(progress(throttle.advance(99, Some(400))), Some(100));
        // Still 25%, so the 1-byte chunk is swallowed.
        assert_eq!(progress(throttle.advance(1, Some(400))), None);
        assert_eq!(progress(throttle.advance(299, Some(400))), Some(400));
    }

    #[test]
    fn stays_silent_without_a_content_length() {
        let mut throttle = PercentThrottle::default();
        assert!(throttle.advance(100, None).is_none());
        assert!(throttle.advance(100, Some(0)).is_none());
    }
}

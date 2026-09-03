//! Self-update: checks the latest GitHub release against the running
//! version, and downloads and swaps in the matching prebuilt binary.

use crate::github::api::GithubApi;
use crate::updater::constants::{GH_REPO, GH_USER};
use crate::updater::release::ReleaseVersionCompareResult;
use futures::StreamExt;
use log::{debug, error, info, warn};
use release::compare_latest_release_to_current_version;
use tokio::fs::File;
use tokio::io;
use tokio::io::AsyncWriteExt;
use tokio::io::BufWriter;

mod constants;
mod error;
pub mod release;

pub async fn cleanup_old_executable() -> anyhow::Result<()> {
    let current_exe = std::env::current_exe()?;
    let current_exe_parent = current_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("current executable path has no parent directory"))?;

    // `_old` is the binary a self-update replaced; `_new` is a download a
    // failed self-update left behind.
    for name in ["rom-converto_old", "rom-converto_new"] {
        let leftover = current_exe_parent.join(name);

        if tokio::fs::try_exists(&leftover).await? {
            tokio::fs::remove_file(&leftover).await?;

            debug!("Removed leftover executable: {leftover:?}");
        }
    }

    Ok(())
}

pub async fn check_for_new_version_and_notify(github_api: &mut GithubApi) -> anyhow::Result<()> {
    let latest_release = github_api
        .get_latest_release_version(GH_USER, GH_REPO)
        .await?;

    let current_version = release::get_current_release_version();

    let compared_version_result =
        compare_latest_release_to_current_version(&latest_release, &current_version);

    match compared_version_result {
        ReleaseVersionCompareResult::OutdatedMajor => {
            warn!(
                "Update available: New major version. Use the self-update command. Major updates may change things significantly. See the Github page for details."
            )
        }
        ReleaseVersionCompareResult::OutdatedMinor => {
            warn!(
                "Update available: New minor version. Use the self-update command. Minor updates add new features and improvements"
            );
        }
        ReleaseVersionCompareResult::OutdatedPatch => {
            warn!(
                "Update available: New patch version. Use the self-update command. Patch updates fix bugs and make small improvements."
            )
        }
        ReleaseVersionCompareResult::EqualOrNewer => {
            debug!(
                "Already on the latest version or a newer one: local {current_version} vs. latest {latest_release}"
            );
        }
    }

    Ok(())
}

pub async fn self_update(github_api: &mut GithubApi) -> anyhow::Result<()> {
    let latest_version = github_api
        .get_latest_release_version(GH_USER, GH_REPO)
        .await?;

    let current_version = release::get_current_release_version();

    let compared_version_result =
        compare_latest_release_to_current_version(&latest_version, &current_version);

    if compared_version_result == ReleaseVersionCompareResult::EqualOrNewer {
        info!("You are already on the latest version: {latest_version}");
        return Ok(());
    }

    info!("New version {latest_version} available, updating");

    let asset_query = match release::get_release_asset_query_for_current_target() {
        Ok(asset_query) => asset_query,
        Err(_) => {
            error!("No prebuild found for your platform, you'll have to build it yourself.");
            return Ok(());
        }
    };

    debug!(
        "Looking for GitHub release asset matching: {}",
        asset_query.expected_name
    );

    let mut file_byte_stream = github_api
        .get_latest_release_file_by_asset_query(GH_USER, GH_REPO, &asset_query)
        .await?;

    let current_exe = std::env::current_exe()?;
    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("current executable path has no parent directory"))?;

    // Staged next to the binary so the final swap is a same-mount rename:
    // a rename from the temp dir fails with EXDEV or ERROR_NOT_SAME_DEVICE
    // when temp lives on tmpfs or another drive.
    let temp_file_path = exe_dir.join("rom-converto_new");

    let file = File::create(&temp_file_path).await?;

    let mut buffered_file = BufWriter::new(file);

    while let Some(item) = file_byte_stream.next().await {
        io::copy(&mut item?.as_ref(), &mut buffered_file).await?;
    }

    buffered_file.flush().await?;
    // The swap below renames onto a path that no longer exists, so nothing
    // orders the data before the rename; a crash could leave an empty binary.
    buffered_file.get_ref().sync_all().await?;
    drop(buffered_file);

    debug!("Downloaded the new release to: {temp_file_path:?}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = tokio::fs::metadata(&temp_file_path).await?.permissions();
        permissions.set_mode(0o755);
        tokio::fs::set_permissions(&temp_file_path, permissions).await?;

        debug!("Marked downloaded release as executable: {temp_file_path:?}");
    }

    let current_exe_renamed = exe_dir.join("rom-converto_old");

    tokio::fs::rename(&current_exe, &current_exe_renamed).await?;

    debug!("Renamed current executable to: {current_exe_renamed:?}");

    if let Err(e) = tokio::fs::rename(&temp_file_path, &current_exe).await {
        // Put the running binary back so the user is never left without one.
        if let Err(rollback) = tokio::fs::rename(&current_exe_renamed, &current_exe).await {
            anyhow::bail!(
                "failed to install the new binary ({e}) and could not restore the old one ({rollback}); rename {} back to {} by hand",
                current_exe_renamed.display(),
                current_exe.display()
            );
        }
        return Err(e.into());
    }

    debug!("Renamed the temporary downloaded file to {current_exe:?}");

    info!(
        "Updated to version {latest_version} (be aware that the old executable will be deleted on next use)"
    );

    Ok(())
}

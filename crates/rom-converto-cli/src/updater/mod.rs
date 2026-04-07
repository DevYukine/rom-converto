use crate::github::api::GithubApi;
use crate::updater::constants::{GH_REPO, GH_USER};
use crate::updater::release::ReleaseVersionCompareResult;
use futures::StreamExt;
use log::{debug, error, info, warn};
use release::compare_latest_release_to_current_version;
use std::env::temp_dir;
use tokio::fs::{File, create_dir_all};
use tokio::io;
use tokio::io::BufWriter;

mod constants;
mod error;
pub mod release;

pub async fn cleanup_old_executable() -> anyhow::Result<()> {
    let current_exe = std::env::current_exe()?;
    let current_exe_parent = current_exe.parent().unwrap();

    debug!("Checking if an outdated executable exists");

    let outdated_exe = current_exe_parent.join("rom-converto_old");

    let exists = tokio::fs::try_exists(&outdated_exe).await?;

    debug!("Outdated executable exists: {exists}");

    if exists {
        tokio::fs::remove_file(&outdated_exe).await?;

        debug!("Removed outdated executable: {outdated_exe:?}");
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

    info!("New version {latest_version} available, updating...");

    let temp_folder_name = temp_dir().join("rom-converto-update");

    create_dir_all(&temp_folder_name).await?;

    debug!("Created temp folder: {temp_folder_name:?}");

    let filename = match release::get_filename_for_current_target_triple() {
        Ok(file_name) => file_name,
        Err(_) => {
            error!("No prebuild found for your platform, you'll have to build it yourself.");
            return Ok(());
        }
    };

    debug!("Got GitHub release filename: {filename}");

    let mut file_byte_stream = github_api
        .get_latest_release_file_by_name(GH_USER, GH_REPO, filename.as_str())
        .await?;

    let temp_file_path = temp_folder_name.join("rom-converto");

    let file = File::create(&temp_file_path).await?;

    let mut buffered_file = BufWriter::new(file);

    while let Some(item) = file_byte_stream.next().await {
        io::copy(&mut item?.as_ref(), &mut buffered_file).await?;
    }

    debug!("Downloaded the new release to: {temp_file_path:?}");

    let current_exe = std::env::current_exe()?;

    let current_exe_renamed = current_exe
        .clone()
        .parent()
        .unwrap()
        .join("rom-converto_old");

    tokio::fs::rename(&current_exe, &current_exe_renamed).await?;

    debug!("Renamed current executable to: {current_exe_renamed:?}");

    tokio::fs::rename(&temp_file_path, &current_exe).await?;

    debug!("Renamed the temporary downloaded file to {current_exe:?}");

    tokio::fs::remove_dir(&temp_folder_name).await?;

    debug!("Removed temp folder: {:?}", &temp_folder_name);

    info!(
        "Updated to version {latest_version} (be aware that the old executable will be deleted on next use)"
    );

    Ok(())
}

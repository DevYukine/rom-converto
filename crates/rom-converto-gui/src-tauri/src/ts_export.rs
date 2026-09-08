//! Regenerates `crates/rom-converto-gui/types/generated` from the Rust
//! types the GUI exchanges with the backend. Run with
//! `cargo test -p rom-converto-gui --features ts-export ts_export`; CI runs
//! the same test and fails on any diff.

use rom_converto_lib::config::UserConfig;
use rom_converto_lib::info::InfoResult;
use rom_converto_lib::runner::cli_echo::{CliEchoManifest, manifest as cli_echo_manifest};
use rom_converto_lib::runner::models::{ProgressEvent, RunRequest, RunResponse, RunRow};
use rom_converto_lib::util::{ReportRecord, ReportTotals};
use ts_rs::{Config, ExportError, TS};

use crate::commands::RunOutcome;

#[test]
fn export_bindings() -> Result<(), ExportError> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../types/generated");
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    // `u64` sizes and counts cross the bridge as JSON numbers, not bigints.
    let cfg = Config::new().with_out_dir(&dir).with_large_int("number");

    InfoResult::export_all(&cfg)?;
    ReportRecord::export_all(&cfg)?;
    ReportTotals::export_all(&cfg)?;
    UserConfig::export_all(&cfg)?;
    RunResponse::export_all(&cfg)?;
    RunRequest::export_all(&cfg)?;
    RunRow::export_all(&cfg)?;
    ProgressEvent::export_all(&cfg)?;
    RunOutcome::export_all(&cfg)?;
    CliEchoManifest::export_all(&cfg)?;

    // The CLI-echo tables are data, not types: the GUI imports the JSON
    // directly. BTreeMaps keep the key order stable across regenerations.
    let json =
        serde_json::to_string_pretty(&cli_echo_manifest()).expect("cli echo manifest serializes");
    std::fs::write(dir.join("cli_echo.json"), json + "\n")?;
    Ok(())
}

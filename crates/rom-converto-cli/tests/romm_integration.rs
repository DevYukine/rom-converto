use std::fs;
use std::process::{Command, Output};

fn bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rom-converto"));
    cmd.env("ROM_CONVERTO_NO_UPDATE_CHECK", "1");
    cmd
}

fn combined(output: &Output) -> String {
    let mut s = String::from_utf8_lossy(&output.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&output.stderr));
    s
}

#[test]
fn capabilities_output_is_json_with_operations_and_info_extensions() {
    let output = bin().arg("capabilities").output().unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json["runner"]["operations"].is_array(), "{json}");
    assert!(json["info_extensions"].is_array(), "{json}");
}

#[test]
fn info_paths_file_reports_per_file_failures_without_failing_the_process() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.iso");
    let unknown = dir.path().join("readme.txt");
    fs::write(&unknown, b"not a rom").unwrap();

    let paths_file = dir.path().join("paths.txt");
    fs::write(
        &paths_file,
        format!("{}\n{}\n", missing.display(), unknown.display()),
    )
    .unwrap();

    let output = bin()
        .args(["info", "--paths-file"])
        .arg(&paths_file)
        .arg("--json")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let entries = json.as_array().unwrap();
    assert_eq!(entries.len(), 2, "{json}");
    assert!(entries.iter().all(|e| e["ok"] == false), "{json}");
}

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
fn verify_recursive_fails_when_a_file_fails() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bad.chd"), b"not a chd").unwrap();

    let output = bin()
        .args(["chd", "verify", "-R"])
        .arg(dir.path())
        .output()
        .unwrap();

    let text = combined(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("0 OK, 1 failed"), "{text}");
}

#[test]
fn migrate_recursive_in_place_fails_when_a_file_fails() {
    let dir = tempfile::tempdir().unwrap();
    // The CHD magic and a v4 version word pass the header peek, so the file
    // is migrated rather than skipped; its zero header length then fails it.
    let mut legacy = b"MComprHD".to_vec();
    legacy.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 4]);
    let source = dir.path().join("old.chd");
    std::fs::write(&source, &legacy).unwrap();

    let output = bin()
        .args(["chd", "migrate", "-R", "--in-place"])
        .arg(dir.path())
        .output()
        .unwrap();

    let text = combined(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("1 of 1 files failed"), "{text}");
    assert_eq!(std::fs::read(&source).unwrap(), legacy);
}

use std::fs;
use std::path::Path;
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

fn no_files_with_ext(dir: &Path, ext: &str) -> bool {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .all(|e| e.path().extension().and_then(|x| x.to_str()) != Some(ext))
}

#[test]
fn cso_compress_dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.iso"), b"not a real iso").unwrap();
    fs::write(dir.path().join("b.iso"), b"also not a real iso").unwrap();

    let output = bin()
        .args(["--dry-run", "cso", "compress", "-R"])
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(no_files_with_ext(dir.path(), "cso"));
    let text = combined(&output);
    assert!(text.contains("would compress"), "{text}");
    assert!(text.contains("dry run:"), "{text}");
}

#[test]
fn cso_compress_dry_run_single_matches_real_path() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("game.iso");
    fs::write(&input, b"x").unwrap();

    let output = bin()
        .args(["--dry-run", "cso", "compress"])
        .arg(&input)
        .arg("--output-dir")
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(!dir.path().join("game.cso").exists());
    let text = combined(&output);
    let expected = dir.path().join("game.cso");
    assert!(text.contains(&expected.display().to_string()), "{text}");
    assert!(text.contains("[new]"), "{text}");
}

#[test]
fn cso_compress_dry_run_overwrite_leaves_existing_file_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("game.iso");
    fs::write(&input, b"x").unwrap();
    let existing = dir.path().join("game.cso");
    fs::write(&existing, b"original contents").unwrap();

    let output = bin()
        .args([
            "--dry-run",
            "cso",
            "compress",
            "--on-conflict",
            "overwrite",
        ])
        .arg(&input)
        .arg("--output-dir")
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(fs::read(&existing).unwrap(), b"original contents");
    let text = combined(&output);
    assert!(text.contains("[overwrite]"), "{text}");
}

#[test]
fn cso_compress_dry_run_output_template_reflects_template() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("game.iso");
    fs::write(&input, b"x").unwrap();

    let output = bin()
        .args(["--dry-run", "cso", "compress"])
        .arg(&input)
        .arg("--output-dir")
        .arg(dir.path())
        .arg("--output-template")
        .arg("sub/{basename}-archived.cso")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(!dir.path().join("sub").exists());
    let text = combined(&output);
    assert!(text.contains("game-archived.cso"), "{text}");
}

#[test]
fn dry_run_missing_input_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist.iso");

    let output = bin()
        .args(["--dry-run", "cso", "compress"])
        .arg(&missing)
        .arg("--output-dir")
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(!output.status.success(), "{}", combined(&output));
}

#[test]
fn chd_compress_dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("disc.iso"), b"fake iso header").unwrap();

    let output = bin()
        .args(["--dry-run", "chd", "compress", "-R"])
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(no_files_with_ext(dir.path(), "chd"));
    assert!(combined(&output).contains("would compress"));
}

#[test]
fn ctr_decrypt_dry_run_recursive_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("game.3ds"), b"fake ctr").unwrap();

    let output = bin()
        .args(["--dry-run", "ctr", "decrypt", "-R"])
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    let text = combined(&output);
    assert!(text.contains("would decrypt"), "{text}");
}

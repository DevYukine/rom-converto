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
    assert!(text.contains("Would compress"), "{text}");
    assert!(text.contains("Dry run:"), "{text}");
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
        .args(["--dry-run", "cso", "compress", "--on-conflict", "overwrite"])
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
    assert!(combined(&output).contains("Would compress"));
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
    assert!(text.contains("Would decrypt"), "{text}");
}

fn iso_payload(len: usize) -> Vec<u8> {
    let mut data = vec![0u8; len];
    let mut state = 0xFEED_F00D_DEAD_BEEFu64;
    for (i, b) in data.iter_mut().enumerate() {
        if (i / 4096) % 2 == 0 {
            *b = (i / 53) as u8;
        } else {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
    }
    data
}

fn corrupt_cso_payload(cso: &Path) {
    let mut bytes = fs::read(cso).unwrap();
    let len = bytes.len();
    let start = len * 2 / 5;
    let end = len * 9 / 10;
    for b in &mut bytes[start..end] {
        *b ^= 0xA5;
    }
    fs::write(cso, &bytes).unwrap();
}

fn make_valid_cso(dir: &Path) -> std::path::PathBuf {
    let input = dir.join("game.iso");
    fs::write(&input, iso_payload(32 * 2048)).unwrap();
    let output = bin()
        .args(["cso", "compress"])
        .arg(&input)
        .arg("--output-dir")
        .arg(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", combined(&output));
    let cso = dir.join("game.cso");
    assert!(cso.exists(), "{}", combined(&output));
    cso
}

#[test]
fn overwrite_invalid_keeps_valid_cso() {
    let dir = tempfile::tempdir().unwrap();
    let cso = make_valid_cso(dir.path());
    let before = fs::read(&cso).unwrap();

    let output = bin()
        .args(["cso", "compress", "--on-conflict", "overwrite-invalid"])
        .arg(dir.path().join("game.iso"))
        .arg("--output-dir")
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(fs::read(&cso).unwrap(), before);
    let text = combined(&output);
    assert!(text.contains("Kept, output verified valid"), "{text}");
}

#[test]
fn overwrite_invalid_rewrites_corrupt_cso() {
    let dir = tempfile::tempdir().unwrap();
    let cso = make_valid_cso(dir.path());
    corrupt_cso_payload(&cso);
    let corrupt = fs::read(&cso).unwrap();

    let output = bin()
        .args(["cso", "compress", "--on-conflict", "overwrite-invalid"])
        .arg(dir.path().join("game.iso"))
        .arg("--output-dir")
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert_ne!(fs::read(&cso).unwrap(), corrupt);
    let text = combined(&output);
    assert!(
        text.contains("Rewriting, output failed verification"),
        "{text}"
    );
}

#[test]
fn overwrite_invalid_writes_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("game.iso");
    fs::write(&input, iso_payload(32 * 2048)).unwrap();

    let output = bin()
        .args(["cso", "compress", "--on-conflict", "overwrite-invalid"])
        .arg(&input)
        .arg("--output-dir")
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(dir.path().join("game.cso").exists());
}

#[test]
fn overwrite_invalid_dry_run_shows_keep_valid() {
    let dir = tempfile::tempdir().unwrap();
    let cso = make_valid_cso(dir.path());
    let before = fs::read(&cso).unwrap();

    let output = bin()
        .args([
            "--dry-run",
            "cso",
            "compress",
            "--on-conflict",
            "overwrite-invalid",
        ])
        .arg(dir.path().join("game.iso"))
        .arg("--output-dir")
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(fs::read(&cso).unwrap(), before);
    let text = combined(&output);
    assert!(text.contains("[keep (valid)]"), "{text}");
}

#[test]
fn overwrite_invalid_dry_run_shows_rewrite_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let cso = make_valid_cso(dir.path());
    corrupt_cso_payload(&cso);
    let corrupt = fs::read(&cso).unwrap();

    let output = bin()
        .args([
            "--dry-run",
            "cso",
            "compress",
            "--on-conflict",
            "overwrite-invalid",
        ])
        .arg(dir.path().join("game.iso"))
        .arg("--output-dir")
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(fs::read(&cso).unwrap(), corrupt);
    let text = combined(&output);
    assert!(text.contains("[rewrite (invalid)]"), "{text}");
}

fn minimal_gamecube_iso(size: usize) -> Vec<u8> {
    let mut data = vec![0u8; size];
    data[0x1C..0x20].copy_from_slice(&0xC2339F3Du32.to_be_bytes());
    for (i, b) in data.iter_mut().enumerate().skip(0x80) {
        *b = (i % 251) as u8;
    }
    data
}

fn make_valid_rvz(dir: &Path) -> std::path::PathBuf {
    let input = dir.join("game.iso");
    fs::write(&input, minimal_gamecube_iso(64 * 1024)).unwrap();
    let output = bin()
        .args(["dol", "compress"])
        .arg(&input)
        .arg("--output-dir")
        .arg(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", combined(&output));
    let rvz = dir.join("game.rvz");
    assert!(rvz.exists(), "{}", combined(&output));
    rvz
}

// The RVZ file-head hash covers bytes 0x00..0x34; flipping a byte past the
// 8-byte magic and before the hash field fails the structural check without
// decompressing any group data.
fn corrupt_rvz(rvz: &Path) {
    let mut bytes = fs::read(rvz).unwrap();
    bytes[0x10] ^= 0xFF;
    fs::write(rvz, &bytes).unwrap();
}

#[test]
fn overwrite_invalid_keeps_valid_rvz() {
    let dir = tempfile::tempdir().unwrap();
    let rvz = make_valid_rvz(dir.path());
    let before = fs::read(&rvz).unwrap();

    let output = bin()
        .args(["dol", "compress", "--on-conflict", "overwrite-invalid"])
        .arg(dir.path().join("game.iso"))
        .arg("--output-dir")
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(fs::read(&rvz).unwrap(), before);
    let text = combined(&output);
    assert!(text.contains("Kept, output verified valid"), "{text}");
}

#[test]
fn overwrite_invalid_rewrites_corrupt_rvz() {
    let dir = tempfile::tempdir().unwrap();
    let rvz = make_valid_rvz(dir.path());
    corrupt_rvz(&rvz);
    let corrupt = fs::read(&rvz).unwrap();

    let output = bin()
        .args(["dol", "compress", "--on-conflict", "overwrite-invalid"])
        .arg(dir.path().join("game.iso"))
        .arg("--output-dir")
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert_ne!(fs::read(&rvz).unwrap(), corrupt);
    let text = combined(&output);
    assert!(
        text.contains("Rewriting, output failed verification"),
        "{text}"
    );
}

#[test]
fn overwrite_invalid_writes_when_rvz_missing() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("game.iso");
    fs::write(&input, minimal_gamecube_iso(64 * 1024)).unwrap();

    let output = bin()
        .args(["dol", "compress", "--on-conflict", "overwrite-invalid"])
        .arg(&input)
        .arg("--output-dir")
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(dir.path().join("game.rvz").exists());
}

#[test]
fn overwrite_invalid_dry_run_rvz_keep_valid() {
    let dir = tempfile::tempdir().unwrap();
    let rvz = make_valid_rvz(dir.path());
    let before = fs::read(&rvz).unwrap();

    let output = bin()
        .args([
            "--dry-run",
            "dol",
            "compress",
            "--on-conflict",
            "overwrite-invalid",
        ])
        .arg(dir.path().join("game.iso"))
        .arg("--output-dir")
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(fs::read(&rvz).unwrap(), before);
    let text = combined(&output);
    assert!(text.contains("[keep (valid)]"), "{text}");
}

#[test]
fn overwrite_invalid_dry_run_rvz_rewrite_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let rvz = make_valid_rvz(dir.path());
    corrupt_rvz(&rvz);
    let corrupt = fs::read(&rvz).unwrap();

    let output = bin()
        .args([
            "--dry-run",
            "dol",
            "compress",
            "--on-conflict",
            "overwrite-invalid",
        ])
        .arg(dir.path().join("game.iso"))
        .arg("--output-dir")
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(fs::read(&rvz).unwrap(), corrupt);
    let text = combined(&output);
    assert!(text.contains("[rewrite (invalid)]"), "{text}");
}

#[test]
fn cdn_to_cia_recursive_dry_run_lists_each_folder() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["title_a", "title_b", "title_c"] {
        fs::create_dir(dir.path().join(name)).unwrap();
    }

    let output = bin()
        .args(["--dry-run", "ctr", "cdn-to-cia", "-R"])
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(no_files_with_ext(dir.path(), "cia"));
    let text = combined(&output);
    assert!(text.contains("title_a.cia"), "{text}");
    assert!(text.contains("title_b.cia"), "{text}");
    assert!(text.contains("title_c.cia"), "{text}");
    assert!(text.contains("Dry run:"), "{text}");
}

#[test]
fn cdn_to_cia_recursive_dry_run_skip_on_existing() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("title_a")).unwrap();
    let existing = dir.path().join("title_a.cia");
    fs::write(&existing, b"original").unwrap();

    let output = bin()
        .args(["--dry-run", "ctr", "cdn-to-cia", "-R"])
        .arg(dir.path())
        .args(["--on-conflict", "skip"])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(fs::read(&existing).unwrap(), b"original");
    assert!(
        combined(&output).contains("[skip]"),
        "{}",
        combined(&output)
    );
}

#[test]
fn cdn_to_cia_recursive_dry_run_error_on_existing() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("title_a")).unwrap();
    fs::write(dir.path().join("title_a.cia"), b"original").unwrap();

    let output = bin()
        .args(["--dry-run", "ctr", "cdn-to-cia", "-R"])
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(!output.status.success(), "{}", combined(&output));
}

#[test]
fn playlist_output_dir_is_created_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Game (Disc 1).cue"), b"x").unwrap();
    fs::write(dir.path().join("Game (Disc 2).cue"), b"x").unwrap();

    let out = dir.path().join("sub").join("nested");

    let output = bin()
        .args(["playlist"])
        .arg(dir.path())
        .arg("--output-dir")
        .arg(&out)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    let m3u = out.join("Game.m3u");
    assert!(m3u.exists(), "{}", combined(&output));
    let contents = fs::read_to_string(&m3u).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 2, "{contents}");
    assert!(lines[0].ends_with("Game (Disc 1).cue"), "{contents}");
    assert!(lines[1].ends_with("Game (Disc 2).cue"), "{contents}");
    assert!(!lines[0].starts_with('/'), "{contents}");
}

#[test]
fn cue_merge_dry_run_notes_companion_bin() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("game.cue");
    fs::write(&input, b"x").unwrap();
    let output = dir.path().join("out.cue");

    let result = bin()
        .arg("--dry-run")
        .args(["cue", "merge"])
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();

    assert!(result.status.success(), "{}", combined(&result));
    assert!(!output.exists());
    assert!(!dir.path().join("out.bin").exists());
    let text = combined(&result);
    let bin_note = format!("(+ {})", dir.path().join("out.bin").display());
    assert!(text.contains(&bin_note), "{text}");
    assert!(text.contains("Would merge"), "{text}");
    assert!(text.contains("[new]"), "{text}");
}

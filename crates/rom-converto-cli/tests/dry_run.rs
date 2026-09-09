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

/// A planned skip is one line: the plan line carries the decision, so the
/// record behind it must not print a second skip note or the runner's
/// "Dry run planned." message.
#[test]
fn cso_compress_dry_run_skip_prints_one_line() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("game.iso");
    fs::write(&input, b"x").unwrap();
    fs::write(dir.path().join("game.cso"), b"original contents").unwrap();

    for args in [
        vec!["--dry-run", "cso", "compress", "--on-conflict", "skip"],
        vec![
            "--dry-run",
            "cso",
            "compress",
            "-R",
            "--on-conflict",
            "skip",
        ],
    ] {
        let recursive = args.contains(&"-R");
        let mut cmd = bin();
        cmd.args(&args);
        if recursive {
            cmd.arg(dir.path());
        } else {
            cmd.arg(&input);
        }
        let output = cmd.output().unwrap();

        assert!(output.status.success(), "{}", combined(&output));
        let text = combined(&output);
        assert!(text.contains("[skip]"), "{args:?}: {text}");
        assert!(!text.contains("Dry run planned."), "{args:?}: {text}");
        assert!(!text.contains("Skipped, output exists"), "{args:?}: {text}");
    }
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
    let existing = dir.path().join("title_a.cia");
    fs::write(&existing, b"original").unwrap();

    let output = bin()
        .args(["--dry-run", "ctr", "cdn-to-cia", "-R"])
        .arg(dir.path())
        .output()
        .unwrap();

    // Every recursive arm reports an existing output under `error` as a
    // warned skip and leaves the rest of the batch planned.
    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(fs::read(&existing).unwrap(), b"original");
    let text = combined(&output);
    assert!(text.contains("output already exists"), "{text}");
    assert!(text.contains("1 skipped"), "{text}");
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

fn write_cue_set(dir: &Path) {
    let bin = dir.join("game.bin");
    fs::write(&bin, vec![0u8; 2352]).unwrap();
    let cue = dir.join("game.cue");
    fs::write(
        &cue,
        "FILE \"game.bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n",
    )
    .unwrap();
}

#[test]
fn cue_to_iso_recursive_dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["disc_a", "disc_b"] {
        let sub = dir.path().join(name);
        fs::create_dir(&sub).unwrap();
        write_cue_set(&sub);
    }

    let output = bin()
        .args(["--dry-run", "cue", "to-iso", "-R"])
        .arg(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(no_files_with_ext(&dir.path().join("disc_a"), "iso"));
    assert!(no_files_with_ext(&dir.path().join("disc_b"), "iso"));
    let text = combined(&output);
    assert!(text.contains("Would to-iso"), "{text}");
    assert!(text.contains("Dry run:"), "{text}");
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

// Regression: `migrate --dry-run` must plan only. It previously ignored the
// global flag and ran a real conversion, writing an RVZ next to the input
// (into a read-only source directory in batch mode).
fn fake_legacy_wia(path: &Path) {
    // The migrate path detects containers by magic, not extension; the 4-byte
    // WIA magic is enough for the planner to recognize the file.
    fs::write(path, [b'W', b'I', b'A', 0x01, 0, 0, 0, 0]).unwrap();
}

fn fake_legacy_gcz(path: &Path) {
    // GCZ magic 0xB10BC001, little-endian; enough for the planner to recognize
    // the file as a GameCube-capable container.
    fs::write(path, [0x01, 0xC0, 0x0B, 0xB1, 0, 0, 0, 0]).unwrap();
}

#[test]
fn dol_migrate_dry_run_recursive_writes_nothing() {
    let src = tempfile::tempdir().unwrap();
    fake_legacy_gcz(&src.path().join("game.gcz"));
    let out = tempfile::tempdir().unwrap();

    let output = bin()
        .args(["--dry-run", "dol", "migrate", "-R"])
        .arg(src.path())
        .arg("-o")
        .arg(out.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(no_files_with_ext(src.path(), "rvz"));
    assert!(no_files_with_ext(out.path(), "rvz"));
    let text = combined(&output);
    assert!(text.contains("Would migrate"), "{text}");
    assert!(text.contains("Dry run:"), "{text}");
}

#[test]
fn dol_migrate_dry_run_rejects_non_legacy_input() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("plain.iso");
    fs::write(&input, vec![0u8; 2048]).unwrap();

    let output = bin()
        .arg("--dry-run")
        .args(["dol", "migrate"])
        .arg(&input)
        .output()
        .unwrap();

    assert!(!output.status.success(), "{}", combined(&output));
    let text = combined(&output);
    assert!(
        text.contains("input is not a GCZ, WIA, or NKit image"),
        "{text}"
    );
}

#[test]
fn dol_migrate_rejects_wia_with_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("game.wia");
    fake_legacy_wia(&input);

    for extra in [&["--dry-run"][..], &[][..]] {
        let output = bin()
            .args(extra)
            .args(["dol", "migrate"])
            .arg(&input)
            .output()
            .unwrap();

        assert!(!output.status.success(), "{}", combined(&output));
        let text = combined(&output);
        assert!(
            text.contains("input is a WIA image; use rvl migrate for Wii disc images"),
            "{text}"
        );
        assert!(no_files_with_ext(dir.path(), "rvz"));
    }
}

#[test]
fn rvl_migrate_dry_run_single_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("game.wia");
    fake_legacy_wia(&input);
    let out = dir.path().join("game.rvz");

    let output = bin()
        .arg("--dry-run")
        .args(["rvl", "migrate"])
        .arg(&input)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(!out.exists());
    let text = combined(&output);
    assert!(text.contains("Would migrate"), "{text}");
    assert!(text.contains("[new]"), "{text}");
}

#[test]
fn explicit_config_shadows_the_ambient_one() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("rom-converto.toml"),
        "[cso]\noutput_dir = \"ambient\"\n",
    )
    .unwrap();
    fs::write(dir.path().join("other.toml"), "[cso]\nblock_size = 32768\n").unwrap();
    fs::write(dir.path().join("game.iso"), b"x").unwrap();

    let ambient = bin()
        .current_dir(dir.path())
        .args(["--dry-run", "cso", "compress", "game.iso"])
        .output()
        .unwrap();
    assert!(ambient.status.success(), "{}", combined(&ambient));
    let ambient_text = combined(&ambient);
    assert!(ambient_text.contains("ambient"), "{ambient_text}");

    let explicit = bin()
        .current_dir(dir.path())
        .args([
            "--config",
            "other.toml",
            "--dry-run",
            "cso",
            "compress",
            "game.iso",
        ])
        .output()
        .unwrap();
    assert!(explicit.status.success(), "{}", combined(&explicit));
    let text = combined(&explicit);
    assert!(text.contains("Would compress"), "{text}");
    assert!(!text.contains("ambient"), "{text}");
}

#[test]
fn dry_run_single_prints_summary_and_writes_report() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("game.iso");
    fs::write(&input, b"x").unwrap();
    let report = dir.path().join("plan.json");

    let output = bin()
        .args(["--dry-run", "cso", "compress"])
        .arg(&input)
        .arg("--report")
        .arg(&report)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(!dir.path().join("game.cso").exists());
    let text = combined(&output);
    assert!(text.contains("Dry run: 1 files planned"), "{text}");
    let plan = fs::read_to_string(&report).unwrap();
    assert!(plan.contains("compress (dry run)"), "{plan}");
}

#[test]
fn already_done_single_reports_the_skip_reason() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("tiny.nds");
    fs::write(&input, vec![0u8; 128]).unwrap();

    let output = bin().args(["ntr", "decrypt"]).arg(&input).output().unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    let text = combined(&output);
    assert!(text.contains("too small for a secure area"), "{text}");
    assert!(!text.contains("1 skipped"), "{text}");
}

#[test]
fn failed_batch_still_writes_its_report() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.iso"), vec![0u8; 4096]).unwrap();
    fs::write(dir.path().join("b.iso"), vec![0u8; 4096]).unwrap();
    let report = dir.path().join("report.json");

    let output = bin()
        .args(["dol", "compress", "-R"])
        .arg(dir.path())
        .arg("--report")
        .arg(&report)
        .output()
        .unwrap();

    assert!(!output.status.success(), "{}", combined(&output));
    let written = fs::read_to_string(&report).unwrap();
    assert!(written.contains("\"failed\""), "{written}");
}

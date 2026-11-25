use std::fs::{self, File};
use std::io::Write;
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

fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
    let opts: zip::write::SimpleFileOptions =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in entries {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

fn cue_sheet(bin_name: &str) -> String {
    format!("FILE \"{bin_name}\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n")
}

fn write_cue_set(dir: &Path, name: &str) {
    fs::create_dir_all(dir).unwrap();
    let bin_name = format!("{name} (Track 1).bin");
    fs::write(dir.join(&bin_name), vec![0u8; 2352 * 300]).unwrap();
    fs::write(dir.join(format!("{name}.cue")), cue_sheet(&bin_name)).unwrap();
}

#[test]
fn batch_removes_cue_set_and_emptied_game_folder() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let out = dir.path().join("out");
    write_cue_set(&lib.join("Game A"), "Game A");
    fs::write(lib.join("Game A").join(".DS_Store"), b"junk").unwrap();

    let output = bin()
        .args(["chd", "compress", "-R"])
        .arg(&lib)
        .arg("--output-dir")
        .arg(&out)
        .arg("--delete-source")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(out.join("Game A").join("Game A.chd").exists());
    assert!(!lib.join("Game A").exists());
    assert!(lib.exists());
}

#[test]
fn output_beside_source_keeps_folder() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let game = lib.join("Game A");
    write_cue_set(&game, "Game A");

    let output = bin()
        .args(["chd", "compress", "-R"])
        .arg(&lib)
        .arg("--delete-source")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(game.join("Game A.chd").exists());
    assert!(!game.join("Game A.cue").exists());
    assert!(!game.join("Game A (Track 1).bin").exists());
}

#[test]
fn leftover_file_keeps_folder() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let out = dir.path().join("out");
    let game = lib.join("Game A");
    write_cue_set(&game, "Game A");
    fs::write(game.join("readme.txt"), b"notes").unwrap();

    let output = bin()
        .args(["chd", "compress", "-R"])
        .arg(&lib)
        .arg("--output-dir")
        .arg(&out)
        .arg("--delete-source")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(!game.join("Game A.cue").exists());
    assert!(!game.join("Game A (Track 1).bin").exists());
    assert!(game.join("readme.txt").exists());
}

#[test]
fn failed_item_keeps_its_sources() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let out = dir.path().join("out");
    write_cue_set(&lib.join("Good"), "Good");
    // The sheet names a bin that is never written, so this item fails.
    fs::create_dir_all(lib.join("Bad")).unwrap();
    fs::write(
        lib.join("Bad").join("Bad.cue"),
        cue_sheet("Bad (Track 1).bin"),
    )
    .unwrap();

    let output = bin()
        .args(["chd", "compress", "-R"])
        .arg(&lib)
        .arg("--output-dir")
        .arg(&out)
        .arg("--delete-source")
        .output()
        .unwrap();

    assert!(!output.status.success(), "{}", combined(&output));
    assert!(lib.join("Bad").join("Bad.cue").exists());
    assert!(!lib.join("Good").exists());
    assert!(out.join("Good").join("Good.chd").exists());
}

#[test]
fn existing_output_skipped_keeps_source() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let out = dir.path().join("out");
    let game = lib.join("Game A");
    write_cue_set(&game, "Game A");
    fs::create_dir_all(out.join("Game A")).unwrap();
    fs::write(out.join("Game A").join("Game A.chd"), b"old").unwrap();

    let output = bin()
        .args(["chd", "compress", "-R"])
        .arg(&lib)
        .arg("--output-dir")
        .arg(&out)
        .args(["--on-conflict", "skip", "--delete-source"])
        .output()
        .unwrap();

    // Clap exits 2 on an unknown flag, which would satisfy the checks below
    // before the flag exists.
    assert_ne!(output.status.code(), Some(2), "{}", combined(&output));
    assert!(game.join("Game A.cue").exists());
    assert!(game.join("Game A (Track 1).bin").exists());
    assert_eq!(
        fs::read(out.join("Game A").join("Game A.chd")).unwrap(),
        b"old"
    );
}

#[test]
fn dry_run_lists_and_deletes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let out = dir.path().join("out");
    let game = lib.join("Game A");
    write_cue_set(&game, "Game A");

    let output = bin()
        .args(["--dry-run", "chd", "compress", "-R"])
        .arg(&lib)
        .arg("--output-dir")
        .arg(&out)
        .arg("--delete-source")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(game.join("Game A.cue").exists());
    assert!(game.join("Game A (Track 1).bin").exists());
    assert!(!out.join("Game A").join("Game A.chd").exists());
    let text = combined(&output).to_lowercase();
    assert!(
        text.lines()
            .any(|line| line.contains("delete") && line.contains("game a (track 1).bin")),
        "{text}"
    );
}

#[test]
fn shared_bin_kept_while_another_sheet_still_needs_it() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let out = dir.path().join("out");
    let disc = lib.join("Disc");
    fs::create_dir_all(&disc).unwrap();
    fs::write(disc.join("shared.bin"), vec![0u8; 2352 * 300]).unwrap();
    fs::write(disc.join("A.cue"), cue_sheet("shared.bin")).unwrap();
    fs::write(
        disc.join("B.cue"),
        "FILE \"shared.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n\
         FILE \"missing.bin\" BINARY\n  TRACK 02 MODE2/2352\n    INDEX 01 00:00:00\n",
    )
    .unwrap();

    let output = bin()
        .args(["chd", "compress", "-R"])
        .arg(&lib)
        .arg("--output-dir")
        .arg(&out)
        .arg("--delete-source")
        .output()
        .unwrap();

    assert!(!output.status.success(), "{}", combined(&output));
    assert!(out.join("Disc").join("A.chd").exists());
    assert!(!disc.join("A.cue").exists());
    assert!(disc.join("shared.bin").exists());
    assert!(disc.join("B.cue").exists());
}

#[test]
fn single_file_run_removes_game_folder() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let out = dir.path().join("out");
    let game = lib.join("Game A");
    write_cue_set(&game, "Game A");

    let output = bin()
        .args(["chd", "compress"])
        .arg(game.join("Game A.cue"))
        .arg("--output-dir")
        .arg(&out)
        .arg("--delete-source")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(out.join("Game A.chd").exists());
    assert!(!game.exists());
    assert!(lib.exists());
}

#[test]
fn single_iso_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    let iso = lib.join("game.iso");
    fs::write(&iso, vec![0u8; 2048 * 64]).unwrap();

    let output = bin()
        .args(["cso", "compress"])
        .arg(&iso)
        .arg("--delete-source")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(lib.join("game.cso").exists());
    assert!(!iso.exists());
}

#[test]
fn zip_of_cue_and_bin_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let out = dir.path().join("out");
    fs::create_dir_all(&lib).unwrap();
    let zip = lib.join("Game.zip");
    let cue = cue_sheet("Game.bin");
    let bin_data = vec![0u8; 2352 * 300];
    write_zip(
        &zip,
        &[
            ("Game.cue", cue.as_bytes()),
            ("Game.bin", bin_data.as_slice()),
        ],
    );

    let output = bin()
        .args(["chd", "compress"])
        .arg(&zip)
        .arg("--output-dir")
        .arg(&out)
        .arg("--delete-source")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(out.join("Game.chd").exists());
    assert!(!zip.exists());
}

#[test]
fn zip_with_extra_entry_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let out = dir.path().join("out");
    fs::create_dir_all(&lib).unwrap();
    let zip = lib.join("Game.zip");
    let cue = cue_sheet("Game.bin");
    let bin_data = vec![0u8; 2352 * 300];
    write_zip(
        &zip,
        &[
            ("Game.cue", cue.as_bytes()),
            ("Game.bin", bin_data.as_slice()),
            ("readme.txt", b"notes"),
        ],
    );

    let output = bin()
        .args(["chd", "compress"])
        .arg(&zip)
        .arg("--output-dir")
        .arg(&out)
        .arg("--delete-source")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(out.join("Game.chd").exists());
    assert!(zip.exists());
}

#[test]
fn verify_after_without_output_check_keeps_source() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let out = dir.path().join("out");
    fs::create_dir_all(&lib).unwrap();
    let iso = lib.join("game.iso");
    fs::write(&iso, vec![0u8; 2048 * 64]).unwrap();
    let made = bin().args(["cso", "compress"]).arg(&iso).output().unwrap();
    assert!(made.status.success(), "{}", combined(&made));
    let cso = lib.join("game.cso");

    let output = bin()
        .args(["cso", "decompress"])
        .arg(&cso)
        .arg("--output-dir")
        .arg(&out)
        .args(["--delete-source", "--verify-after"])
        .output()
        .unwrap();

    // Clap exits 2 on an unknown flag, which would satisfy the checks below
    // before the flags exist.
    assert_ne!(output.status.code(), Some(2), "{}", combined(&output));
    assert!(!output.status.success(), "{}", combined(&output));
    assert!(cso.exists());
}

#[test]
fn verify_after_with_passing_check_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    let out = dir.path().join("out");
    let game = lib.join("Game A");
    write_cue_set(&game, "Game A");

    let output = bin()
        .args(["chd", "compress"])
        .arg(game.join("Game A.cue"))
        .arg("--output-dir")
        .arg(&out)
        .args(["--delete-source", "--verify-after"])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(!game.exists());
}

#[test]
fn commands_that_cannot_replace_input_reject_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("game.chd");

    for args in [["chd", "extract"], ["cue", "to-iso"], ["nx", "merge"]] {
        let output = bin()
            .args(args)
            .arg(&path)
            .arg("--delete-source")
            .output()
            .unwrap();

        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            combined(&output)
        );
        assert!(
            combined(&output).contains("--delete-source"),
            "{args:?}: {}",
            combined(&output)
        );
    }
}

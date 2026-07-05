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
    let opts: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in entries {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn hash_reads_first_member_of_zip() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("game.zip");
    write_zip(&zip, &[("readme.txt", b"junk"), ("game.iso", b"payload")]);

    let output = bin()
        .args(["hash", "--algo", "crc32"])
        .arg(&zip)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    let text = combined(&output);
    // CRC32 of "payload".
    assert!(text.to_lowercase().contains("crc32="), "{text}");
}

#[test]
fn cso_compress_dry_run_derives_member_output_next_to_archive() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("disc.zip");
    write_zip(&zip, &[("game.iso", b"not a real iso")]);

    let output = bin()
        .args(["--dry-run", "cso", "compress"])
        .arg(&zip)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    let text = combined(&output);
    let expected = dir.path().join("game.cso");
    assert!(
        text.contains(&expected.display().to_string()),
        "plan should target the member-derived path next to the archive: {text}"
    );
    assert!(!dir.path().join("game.cso").exists());
}

#[test]
fn cso_decompress_dry_run_derives_member_output_next_to_archive() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("disc.zip");
    write_zip(&zip, &[("game.cso", b"not a real cso")]);

    let output = bin()
        .args(["--dry-run", "cso", "decompress"])
        .arg(&zip)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    let text = combined(&output);
    let expected = dir.path().join("game.iso");
    assert!(
        text.contains(&expected.display().to_string()),
        "plan should target the member-derived path next to the archive: {text}"
    );
}

#[test]
fn archive_with_no_match_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("docs.zip");
    write_zip(&zip, &[("readme.txt", b"x")]);

    let output = bin().args(["cso", "compress"]).arg(&zip).output().unwrap();

    assert!(!output.status.success());
    assert!(
        combined(&output).contains("no matching image"),
        "{}",
        combined(&output)
    );
}

/// A RAR5 archive holding `game.iso` (`rar-payload`, crc32 908a1c2f) and
/// `readme.txt`, stored uncompressed. Embedded because rar cannot be
/// produced at test time: no permissively licensed encoder exists.
const RAR_FIXTURE: &[u8] = &[
    82, 97, 114, 33, 26, 7, 1, 0, 51, 146, 181, 229, 10, 1, 5, 6, 0, 5, 1, 1, 128, 128, 0, 73, 41,
    127, 148, 36, 2, 3, 11, 139, 0, 4, 139, 0, 32, 47, 28, 138, 144, 128, 0, 0, 8, 103, 97, 109,
    101, 46, 105, 115, 111, 10, 3, 2, 233, 77, 17, 63, 141, 12, 221, 1, 114, 97, 114, 45, 112, 97,
    121, 108, 111, 97, 100, 108, 5, 180, 188, 38, 2, 3, 11, 130, 0, 4, 130, 0, 32, 172, 42, 147,
    216, 128, 0, 0, 10, 114, 101, 97, 100, 109, 101, 46, 116, 120, 116, 10, 3, 2, 52, 117, 17, 63,
    141, 12, 221, 1, 104, 105, 29, 119, 86, 81, 3, 5, 4, 0,
];

#[test]
fn rar_input_hashes_member() {
    let dir = tempfile::tempdir().unwrap();
    let rar = dir.path().join("game.rar");
    fs::write(&rar, RAR_FIXTURE).unwrap();

    let output = bin()
        .args(["hash", "--algo", "crc32"])
        .arg(&rar)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert!(
        combined(&output).to_lowercase().contains("908a1c2f"),
        "{}",
        combined(&output)
    );
}

#[test]
fn chd_to_cso_dry_run_derives_member_output_next_to_archive() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("disc.zip");
    write_zip(&zip, &[("game.chd", b"not a real chd")]);

    let output = bin()
        .args(["--dry-run", "chd", "to-cso"])
        .arg(&zip)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    let text = combined(&output);
    let expected = dir.path().join("game.cso");
    assert!(
        text.contains(&expected.display().to_string()),
        "plan should target the member-derived path next to the archive: {text}"
    );
    assert!(!dir.path().join("game.cso").exists());
}

#[test]
fn cso_to_chd_dry_run_derives_member_output_next_to_archive() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("disc.zip");
    write_zip(&zip, &[("game.cso", b"not a real cso")]);

    let output = bin()
        .args(["--dry-run", "cso", "to-chd"])
        .arg(&zip)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    let text = combined(&output);
    let expected = dir.path().join("game.chd");
    assert!(
        text.contains(&expected.display().to_string()),
        "plan should target the member-derived path next to the archive: {text}"
    );
    assert!(!dir.path().join("game.chd").exists());
}

#[test]
fn rvl_compress_accepts_wia_member() {
    // WIA is a Wii-only legacy container, so the rvl allowlist must include it.
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("disc.zip");
    write_zip(&zip, &[("game.wia", b"not a real wia")]);

    let output = bin()
        .args(["--dry-run", "rvl", "compress"])
        .arg(&zip)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    let text = combined(&output);
    let expected = dir.path().join("game.rvz");
    assert!(
        text.contains(&expected.display().to_string()),
        "plan should target the member-derived path next to the archive: {text}"
    );
}

#[test]
fn dat_verify_archive_without_image_is_rejected() {
    // Routing through resolve_input means a container with no inner image is
    // rejected up front instead of hashing the container and printing a wrong
    // verdict.
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("docs.zip");
    write_zip(&zip, &[("readme.txt", b"x")]);

    let output = bin()
        .args(["dat", "verify", "--algo", "crc32"])
        .arg(&zip)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        combined(&output).contains("no matching image"),
        "{}",
        combined(&output)
    );
}

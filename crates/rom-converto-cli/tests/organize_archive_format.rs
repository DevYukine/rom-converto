use std::fs::{self, File};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
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

fn organize(input: &Path, output_dir: &Path, extra: &[&str]) -> Output {
    bin()
        .arg("organize")
        .arg(input)
        .arg("--output-dir")
        .arg(output_dir)
        .args(extra)
        .output()
        .unwrap()
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

fn write_7z(path: &Path, entries: &[(&str, &[u8])]) {
    let mut writer = sevenz_rust2::ArchiveWriter::create(path).unwrap();
    for (name, data) in entries {
        writer
            .push_archive_entry::<&[u8]>(sevenz_rust2::ArchiveEntry::new_file(name), Some(data))
            .unwrap();
    }
    writer.finish().unwrap();
}

fn write_tar_gz(path: &Path, entries: &[(&str, &[u8])]) {
    let gz =
        flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        tar.append_data(&mut header, name, *data).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
}

/// A GBA cartridge image: the header organize parses (fixed 0x96 at 0xB2,
/// title at 0xA0), then 4 KiB of varied bytes so the archives have real
/// data to compress.
fn gba_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x1000];
    rom[0xA0..0xA8].copy_from_slice(b"TESTGAME");
    rom[0xB2] = 0x96;
    for (i, byte) in rom.iter_mut().enumerate().skip(0xC0) {
        *byte = (i.wrapping_mul(31) ^ (i >> 3)) as u8;
    }
    rom
}

fn gba_member(rom: &[u8]) -> Vec<(String, Vec<u8>)> {
    vec![("Game.gba".to_string(), rom.to_vec())]
}

fn zip_members(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut archive = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
    let mut members = Vec::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        members.push((file.name().unwrap().into_owned(), bytes));
    }
    members
}

fn sevenz_members(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut archive =
        sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty()).unwrap();
    let mut members = Vec::new();
    archive
        .for_each_entries(|entry, reader| {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            members.push((entry.name().to_string(), bytes));
            Ok(true)
        })
        .unwrap();
    members
}

fn tar_members(reader: impl Read) -> Vec<(String, Vec<u8>)> {
    let mut archive = tar::Archive::new(reader);
    let mut members = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        members.push((name, bytes));
    }
    members
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else {
            files.push(path);
        }
    }
    files
}

fn files_ending_with(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    files_under(dir)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(suffix)
        })
        .collect()
}

/// The one output with this suffix, wherever its console folder put it.
fn only_output(dir: &Path, suffix: &str) -> PathBuf {
    let mut found = files_ending_with(dir, suffix);
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0)
}

#[test]
fn archive_format_7z_writes_a_7z_holding_the_rom() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let rom = gba_rom();
    fs::write(input.path().join("Game.gba"), &rom).unwrap();

    let output = organize(input.path(), out.path(), &["--archive-format", "7z"]);

    assert!(output.status.success(), "{}", combined(&output));
    let archive = only_output(out.path(), ".7z");
    assert_eq!(archive.file_name().unwrap(), "Game.7z");
    assert_eq!(sevenz_members(&archive), gba_member(&rom));
    assert!(files_ending_with(out.path(), ".zip").is_empty());
}

#[test]
fn archive_format_tar_writes_a_tar_holding_the_rom() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let rom = gba_rom();
    fs::write(input.path().join("Game.gba"), &rom).unwrap();

    let output = organize(input.path(), out.path(), &["--archive-format", "tar"]);

    assert!(output.status.success(), "{}", combined(&output));
    let archive = only_output(out.path(), ".tar");
    assert_eq!(archive.file_name().unwrap(), "Game.tar");
    assert_eq!(tar_members(File::open(&archive).unwrap()), gba_member(&rom));
    assert!(files_ending_with(out.path(), ".zip").is_empty());
}

#[test]
fn archive_format_tar_gz_writes_a_tar_gz_holding_the_rom() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let rom = gba_rom();
    fs::write(input.path().join("Game.gba"), &rom).unwrap();

    let output = organize(input.path(), out.path(), &["--archive-format", "tar.gz"]);

    assert!(output.status.success(), "{}", combined(&output));
    let archive = only_output(out.path(), ".tar.gz");
    assert_eq!(archive.file_name().unwrap(), "Game.tar.gz");
    assert_eq!(
        tar_members(flate2::read::GzDecoder::new(File::open(&archive).unwrap())),
        gba_member(&rom)
    );
    assert!(files_ending_with(out.path(), ".zip").is_empty());
}

#[test]
fn new_archive_outputs_are_reproducible() {
    let input = tempfile::tempdir().unwrap();
    fs::write(input.path().join("Game.gba"), gba_rom()).unwrap();

    for format in ["7z", "tar", "tar.gz"] {
        let suffix = format!(".{format}");
        let mut written = Vec::new();
        for _ in 0..2 {
            let out = tempfile::tempdir().unwrap();
            let output = organize(input.path(), out.path(), &["--archive-format", format]);
            assert!(output.status.success(), "{format}: {}", combined(&output));
            written.push(fs::read(only_output(out.path(), &suffix)).unwrap());
        }
        assert_eq!(written[0], written[1], "{format}");
    }
}

#[test]
fn zip_input_is_repacked_to_7z() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let rom = gba_rom();
    write_zip(
        &input.path().join("Game.zip"),
        &[("Game.gba", rom.as_slice())],
    );

    let output = organize(input.path(), out.path(), &["--archive-format", "7z"]);

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        sevenz_members(&only_output(out.path(), ".7z")),
        gba_member(&rom)
    );
}

#[test]
fn seven_z_input_is_repacked_to_zip_by_default() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let rom = gba_rom();
    write_7z(
        &input.path().join("Game.7z"),
        &[("Game.gba", rom.as_slice())],
    );

    let output = organize(input.path(), out.path(), &[]);

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        zip_members(&only_output(out.path(), ".zip")),
        gba_member(&rom)
    );
}

#[test]
fn tar_gz_input_is_repacked_to_zip_by_default() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let rom = gba_rom();
    write_tar_gz(
        &input.path().join("Game.tar.gz"),
        &[("Game.gba", rom.as_slice())],
    );

    let output = organize(input.path(), out.path(), &[]);

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        zip_members(&only_output(out.path(), ".zip")),
        gba_member(&rom)
    );
}

#[test]
fn tar_gz_input_with_planned_member_is_copied_through() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let source = input.path().join("Game.tar.gz");
    write_tar_gz(&source, &[("Game.gba", gba_rom().as_slice())]);

    let output = organize(input.path(), out.path(), &["--archive-format", "tar.gz"]);

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        fs::read(only_output(out.path(), ".tar.gz")).unwrap(),
        fs::read(&source).unwrap()
    );
}

#[test]
fn seven_z_input_with_planned_member_is_copied_through() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let source = input.path().join("Game.7z");
    // COPY content keeps this input apart from the LZMA2 archive a repack
    // writes, so identical bytes can only come from a true copy-through.
    let mut writer = sevenz_rust2::ArchiveWriter::create(&source).unwrap();
    writer.set_content_methods(vec![sevenz_rust2::EncoderConfiguration::new(
        sevenz_rust2::EncoderMethod::COPY,
    )]);
    writer
        .push_archive_entry::<&[u8]>(
            sevenz_rust2::ArchiveEntry::new_file("Game.gba"),
            Some(gba_rom().as_slice()),
        )
        .unwrap();
    writer.finish().unwrap();

    let output = organize(input.path(), out.path(), &["--archive-format", "7z"]);

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        fs::read(only_output(out.path(), ".7z")).unwrap(),
        fs::read(&source).unwrap()
    );
}

#[test]
fn seven_z_input_with_a_second_entry_is_repacked() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let rom = gba_rom();
    let source = input.path().join("Game.7z");
    write_7z(
        &source,
        &[("Game.gba", rom.as_slice()), ("readme.txt", b"notes")],
    );

    let output = organize(input.path(), out.path(), &["--archive-format", "7z"]);

    assert!(output.status.success(), "{}", combined(&output));
    let archive = only_output(out.path(), ".7z");
    assert_ne!(fs::read(&archive).unwrap(), fs::read(&source).unwrap());
    assert_eq!(sevenz_members(&archive), gba_member(&rom));
}

#[test]
fn seven_z_input_with_the_member_in_a_folder_is_repacked() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let rom = gba_rom();
    write_7z(
        &input.path().join("Game.7z"),
        &[("dir/Game.gba", rom.as_slice())],
    );

    let output = organize(input.path(), out.path(), &["--archive-format", "7z"]);

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        sevenz_members(&only_output(out.path(), ".7z")),
        gba_member(&rom)
    );
}

#[test]
fn verify_after_passes_for_7z_output() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let rom = gba_rom();
    fs::write(input.path().join("Game.gba"), &rom).unwrap();

    let output = organize(
        input.path(),
        out.path(),
        &["--archive-format", "7z", "--verify-after"],
    );

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        sevenz_members(&only_output(out.path(), ".7z")),
        gba_member(&rom)
    );
}

#[test]
fn overwrite_invalid_rewrites_a_corrupt_7z_and_keeps_a_valid_one() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let rom = gba_rom();
    fs::write(input.path().join("Game.gba"), &rom).unwrap();

    let first = organize(input.path(), out.path(), &["--archive-format", "7z"]);
    assert!(first.status.success(), "{}", combined(&first));
    let archive = only_output(out.path(), ".7z");
    fs::write(&archive, b"not a 7z archive").unwrap();

    let args = [
        "--archive-format",
        "7z",
        "--on-conflict",
        "overwrite-invalid",
    ];
    let repaired = organize(input.path(), out.path(), &args);
    assert!(repaired.status.success(), "{}", combined(&repaired));
    assert_eq!(sevenz_members(&archive), gba_member(&rom));

    let kept = fs::read(&archive).unwrap();
    let before = fs::metadata(&archive).unwrap();
    let again = organize(input.path(), out.path(), &args);
    assert!(again.status.success(), "{}", combined(&again));
    assert!(
        combined(&again).contains("existing output verified valid"),
        "{}",
        combined(&again)
    );
    assert_eq!(fs::read(&archive).unwrap(), kept);
    // The output is deterministic, so identical bytes cannot show a rewrite.
    // A rewrite publishes a new file, which changes the inode and mtime.
    let after = fs::metadata(&archive).unwrap();
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    #[cfg(unix)]
    assert_eq!(before.ino(), after.ino());
}

#[test]
fn move_deletes_the_zip_source_after_writing_the_7z() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let rom = gba_rom();
    let source = input.path().join("Game.zip");
    write_zip(&source, &[("Game.gba", rom.as_slice())]);

    let output = organize(
        input.path(),
        out.path(),
        &["--move", "--archive-format", "7z"],
    );

    assert!(output.status.success(), "{}", combined(&output));
    assert!(!source.exists());
    assert_eq!(
        sevenz_members(&only_output(out.path(), ".7z")),
        gba_member(&rom)
    );
}

#[test]
fn zip_exclude_matches_the_planned_7z_path() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let rom = gba_rom();
    fs::write(input.path().join("Game.gba"), &rom).unwrap();

    let output = organize(
        input.path(),
        out.path(),
        &["--archive-format", "7z", "--zip-exclude", "**/*.7z"],
    );

    assert!(output.status.success(), "{}", combined(&output));
    assert!(files_ending_with(out.path(), ".7z").is_empty());
    let copy = only_output(out.path(), ".gba");
    assert_eq!(copy.file_name().unwrap(), "Game.gba");
    assert_eq!(fs::read(&copy).unwrap(), rom);
}

#[test]
fn rename_keeps_the_tar_gz_suffix_together() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fs::write(input.path().join("Game.gba"), gba_rom()).unwrap();

    let first = organize(input.path(), out.path(), &["--archive-format", "tar.gz"]);
    assert!(first.status.success(), "{}", combined(&first));
    let archive = only_output(out.path(), ".tar.gz");

    let second = organize(
        input.path(),
        out.path(),
        &["--archive-format", "tar.gz", "--on-conflict", "rename"],
    );

    assert!(second.status.success(), "{}", combined(&second));
    assert!(archive.parent().unwrap().join("Game (1).tar.gz").exists());
}

#[test]
fn config_archive_format_is_used() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let config = config_dir.path().join("rom-converto.toml");
    let rom = gba_rom();
    fs::write(input.path().join("Game.gba"), &rom).unwrap();
    fs::write(&config, "[organize]\narchive_format = \"7z\"\n").unwrap();

    let output = bin()
        .arg("--config")
        .arg(&config)
        .arg("organize")
        .arg(input.path())
        .arg("--output-dir")
        .arg(out.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        sevenz_members(&only_output(out.path(), ".7z")),
        gba_member(&rom)
    );
}

#[test]
fn archive_format_rar_is_rejected() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fs::write(input.path().join("Game.gba"), gba_rom()).unwrap();

    let output = organize(input.path(), out.path(), &["--archive-format", "rar"]);

    assert!(!output.status.success(), "{}", combined(&output));
    // An unknown-flag error also exits non-zero, so the message has to name
    // the rejected value to show the flag exists and refused it.
    assert!(combined(&output).contains("'rar'"), "{}", combined(&output));
    assert!(files_under(out.path()).is_empty());
}

#[test]
fn zip_format_still_defaults_to_torrentzip() {
    let input = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fs::write(input.path().join("Game.gba"), gba_rom()).unwrap();

    let output = organize(input.path(), out.path(), &[]);

    assert!(output.status.success(), "{}", combined(&output));
    let zip_path = only_output(out.path(), ".zip");
    assert_eq!(zip_path.file_name().unwrap(), "Game.zip");
    let archive = zip::ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
    assert!(archive.comment().starts_with(b"TORRENTZIPPED-"));
}

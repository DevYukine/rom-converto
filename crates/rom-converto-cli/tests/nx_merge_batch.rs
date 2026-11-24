//! `nx merge` over game folders. The dry run never opens its inputs, so the
//! fixtures are placeholder bytes. HOME points at an empty directory so a
//! `prod.keys` on the machine running the tests is never read.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rom-converto"));
    cmd.env("ROM_CONVERTO_NO_UPDATE_CHECK", "1")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("XDG_CONFIG_HOME");
    cmd
}

fn combined(output: &Output) -> String {
    let mut s = String::from_utf8_lossy(&output.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&output.stderr));
    s
}

fn write(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"placeholder").unwrap();
}

struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    base: PathBuf,
}

/// Game A (base + update), Game B (xci + DLC subfolder), a loose NSP, an
/// empty folder and a folder without any container.
fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let base = root.path().join("base");
    write(&base.join("Game A").join("base.nsp"));
    write(&base.join("Game A").join("update.nsp"));
    write(&base.join("Game B").join("game.xci"));
    write(&base.join("Game B").join("DLC").join("dlc.nsp"));
    write(&base.join("loose.nsp"));
    fs::create_dir(base.join("Empty")).unwrap();
    write(&base.join("Notes").join("readme.txt"));
    Fixture {
        _root: root,
        home,
        base,
    }
}

fn dry_run(fx: &Fixture, args: &[&str], paths: &[&Path]) -> Output {
    let mut cmd = bin(&fx.home);
    cmd.arg("--dry-run").args(["nx", "merge"]).args(args);
    cmd.args(paths);
    cmd.output().unwrap()
}

fn files_with_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some(ext))
        .collect()
}

fn assert_no_stray_games(text: &str) {
    for name in ["loose (Merged)", "Empty (Merged)", "Notes (Merged)"] {
        assert!(!text.contains(name), "{name} planned: {text}");
    }
}

#[test]
fn batch_dry_run_plans_one_output_per_game_folder() {
    let fx = fixture();
    let output = dry_run(&fx, &["-R"], &[&fx.base]);

    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("Game A (Merged).nsp"), "{text}");
    assert!(text.contains("Game B (Merged).nsp"), "{text}");
    assert_no_stray_games(&text);
    assert!(files_with_ext(&fx.base, "nsp").len() == 1, "only loose.nsp");
    assert!(!fx.base.join("Game A (Merged).nsp").exists());
    assert!(!fx.base.join("Game B (Merged).nsp").exists());
}

#[test]
fn batch_dry_run_skips_a_game_whose_output_exists() {
    let fx = fixture();
    let existing = fx.base.join("Game A (Merged).nsp");
    fs::write(&existing, b"already merged").unwrap();

    let output = dry_run(&fx, &["-R"], &[&fx.base]);

    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("Game B (Merged).nsp"), "{text}");
    assert_no_stray_games(&text);
    assert_eq!(fs::read(&existing).unwrap(), b"already merged");
    // A game that is skipped is not planned as a write.
    for line in text.lines().filter(|l| l.contains("Game A (Merged).nsp")) {
        assert!(
            !line.contains("[new]") && !line.contains("[overwrite]") && !line.contains("[rename"),
            "Game A planned as a write: {line}"
        );
    }
}

#[test]
fn batch_dry_run_places_outputs_in_output_dir_and_ignores_it_as_a_game() {
    let fx = fixture();
    let out = fx.base.join("merged");
    write(&out.join("X (Merged).nsp"));

    let output = dry_run(
        &fx,
        &["-R", "--output-dir", out.to_str().unwrap()],
        &[&fx.base],
    );

    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(!text.contains("merged (Merged)"), "{text}");
    assert_no_stray_games(&text);
    for game in ["Game A", "Game B"] {
        let planned = out.join(format!("{game} (Merged).nsp"));
        assert!(text.contains(&planned.display().to_string()), "{text}");
    }
    assert!(!fx.base.join("Game A (Merged).nsp").exists());
}

#[test]
fn single_game_folder_dry_run_plans_one_output_beside_the_folder() {
    let fx = fixture();
    let game = fx.base.join("Game A");

    let output = dry_run(&fx, &[], &[&game]);

    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    let planned = fx.base.join("Game A (Merged).nsp");
    assert!(text.contains(&planned.display().to_string()), "{text}");
    assert!(!game.join("Game A (Merged).nsp").exists());
    assert!(!planned.exists());
}

#[test]
fn game_folder_without_a_container_is_an_error() {
    let fx = fixture();
    for folder in ["Empty", "Notes"] {
        let output = dry_run(&fx, &[], &[&fx.base.join(folder)]);
        assert!(!output.status.success(), "{folder}: {}", combined(&output));
    }
}

#[test]
fn batch_dry_run_with_xci_format_plans_xci_outputs() {
    let fx = fixture();
    let output = dry_run(&fx, &["-R", "--format", "xci"], &[&fx.base]);

    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("Game A (Merged).xci"), "{text}");
    assert!(text.contains("Game B (Merged).xci"), "{text}");
    assert!(!text.contains("(Merged).nsp"), "{text}");
    assert_no_stray_games(&text);
}

#[test]
fn recursive_merge_takes_exactly_one_directory_and_no_output_file() {
    let fx = fixture();
    // The batch form itself must work, or the rejections below prove nothing.
    let ok = dry_run(&fx, &["-R"], &[&fx.base]);
    assert!(ok.status.success(), "{}", combined(&ok));

    let other = fx.base.join("Game B");
    let two_inputs = dry_run(&fx, &["-R"], &[&fx.base, &other]);
    assert!(!two_inputs.status.success(), "{}", combined(&two_inputs));

    let out_file = fx.base.join("out.nsp");
    let with_output = dry_run(&fx, &["-R", "-o", out_file.to_str().unwrap()], &[&fx.base]);
    assert!(!with_output.status.success(), "{}", combined(&with_output));
    assert!(!out_file.exists());
}

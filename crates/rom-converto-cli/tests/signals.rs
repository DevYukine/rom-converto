#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);

/// A sparse file no command gets through within [`TIMEOUT`].
fn huge_file(dir: &Path, len: u64) -> PathBuf {
    let path = dir.join("big.iso");
    std::fs::File::create(&path).unwrap().set_len(len).unwrap();
    path
}

/// A running CLI and the lines it writes to stderr.
struct Run {
    child: Child,
    stderr: Receiver<String>,
}

/// Starts the CLI with `args`, through `nohup` when `nohup` is set, and
/// returns once it listens for signals.
fn start(args: &[&str], nohup: bool) -> Run {
    let exe = env!("CARGO_BIN_EXE_rom-converto");
    let mut cmd = if nohup {
        let mut cmd = Command::new("nohup");
        cmd.arg(exe);
        cmd
    } else {
        Command::new(exe)
    };
    // A test runner started under nohup would pass its ignored SIGHUP on.
    // SAFETY: signal() is async-signal-safe, as pre_exec requires.
    unsafe {
        cmd.pre_exec(|| {
            libc::signal(libc::SIGHUP, libc::SIG_DFL);
            Ok(())
        });
    }
    let mut child = cmd
        .env("ROM_CONVERTO_NO_UPDATE_CHECK", "1")
        .args(["-v", "--no-cache"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pipe = child.stderr.take().unwrap();
    let (lines, stderr) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    loop {
        match stderr.recv_timeout(TIMEOUT) {
            Ok(line) if line.contains("Listening for signals") => return Run { child, stderr },
            Ok(_) => {}
            Err(err) => {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("the CLI never listened for signals: {err}");
            }
        }
    }
}

fn send(run: &Run, signal: &str) {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(run.child.id().to_string())
        .status()
        .unwrap();
    assert!(status.success());
}

/// The exit status and the rest of stderr, killing the run if it outlives
/// [`TIMEOUT`].
fn finish(mut run: Run) -> (ExitStatus, String) {
    let started = Instant::now();
    let status = loop {
        if let Some(status) = run.child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > TIMEOUT {
            run.child.kill().unwrap();
            panic!("still running after {TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    (status, run.stderr.iter().collect::<Vec<_>>().join("\n"))
}

fn hash(dir: &Path, nohup: bool) -> Run {
    let input = huge_file(dir, 64 << 30);
    start(&["hash", input.to_str().unwrap()], nohup)
}

#[test]
fn sigterm_cancels_like_ctrl_c_even_when_sent_twice() {
    let dir = tempfile::tempdir().unwrap();
    let run = hash(dir.path(), false);
    send(&run, "TERM");
    std::thread::sleep(Duration::from_millis(30));
    send(&run, "TERM");
    let (status, stderr) = finish(run);
    assert_eq!(status.code(), Some(143), "{stderr}");
    assert!(stderr.contains("Cancelled"), "{stderr}");
}

#[test]
fn sighup_cancels_with_its_own_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let run = hash(dir.path(), false);
    send(&run, "HUP");
    let (status, stderr) = finish(run);
    assert_eq!(status.code(), Some(129), "{stderr}");
    assert!(stderr.contains("Cancelled"), "{stderr}");
}

#[test]
fn a_run_under_nohup_ignores_sighup() {
    let dir = tempfile::tempdir().unwrap();
    let run = hash(dir.path(), true);
    send(&run, "HUP");
    std::thread::sleep(Duration::from_millis(500));
    send(&run, "TERM");
    let (status, stderr) = finish(run);
    assert_eq!(status.code(), Some(143), "{stderr}");
}

#[test]
fn sigterm_leaves_no_partial_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = huge_file(dir.path(), 2 << 30);
    let output = dir.path().join("big.cso");
    let run = start(
        &[
            "--skip-space-check",
            "cso",
            "compress",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ],
        false,
    );
    let entries = || std::fs::read_dir(dir.path()).unwrap().count();
    let started = Instant::now();
    while entries() < 2 {
        assert!(started.elapsed() < TIMEOUT, "no scratch output appeared");
        std::thread::sleep(Duration::from_millis(10));
    }
    send(&run, "TERM");
    let (status, stderr) = finish(run);
    assert_eq!(status.code(), Some(143), "{stderr}");
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, ["big.iso"], "{stderr}");
}

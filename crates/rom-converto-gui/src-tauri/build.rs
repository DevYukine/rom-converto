fn main() {
    tauri_build::build();

    let is_release = std::env::var("ROM_CONVERTO_RELEASE").is_ok();
    let semver = std::env::var("CARGO_PKG_VERSION").unwrap();
    let display = display_version(is_release, &semver, git_short_hash().as_deref());
    println!("cargo:rustc-env=ROM_CONVERTO_DISPLAY_VERSION={display}");
    println!("cargo:rerun-if-env-changed=ROM_CONVERTO_RELEASE");
    println!("cargo:rerun-if-changed=.git/HEAD");
}

// Mirror of the CLI version logic tested in rom-converto-cli; build scripts cannot share crate code.
fn display_version(is_release: bool, semver: &str, short_hash: Option<&str>) -> String {
    if is_release {
        return semver.to_string();
    }
    match short_hash {
        Some(h) if !h.is_empty() => format!("dev-{h}"),
        _ => semver.to_string(),
    }
}

fn git_short_hash() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if hash.is_empty() { None } else { Some(hash) }
}

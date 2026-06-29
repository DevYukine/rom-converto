#![cfg(test)]

fn display_version(is_release: bool, semver: &str, short_hash: Option<&str>) -> String {
    if is_release {
        return semver.to_string();
    }
    match short_hash {
        Some(h) if !h.is_empty() => format!("dev-{h}"),
        _ => semver.to_string(),
    }
}

#[test]
fn release_returns_semver() {
    assert_eq!(display_version(true, "0.12.0", Some("2dd4ee7")), "0.12.0");
}

#[test]
fn dev_with_hash_returns_dev_hash() {
    assert_eq!(display_version(false, "0.12.0", Some("2dd4ee7")), "dev-2dd4ee7");
}

#[test]
fn dev_without_hash_falls_back_to_semver() {
    assert_eq!(display_version(false, "0.12.0", None), "0.12.0");
}

#[test]
fn dev_with_empty_hash_falls_back_to_semver() {
    assert_eq!(display_version(false, "0.12.0", Some("")), "0.12.0");
}

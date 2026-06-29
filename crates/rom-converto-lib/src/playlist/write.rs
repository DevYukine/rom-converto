use super::detect::group_disc_files;
use crate::util::fs::collect_files_with_exts;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistMode {
    Multiple,
    Always,
}

pub struct PlaylistOptions<'a> {
    pub scan_dir: &'a Path,
    pub output_dir: Option<&'a Path>,
    pub extensions: &'a [&'a str],
    pub mode: PlaylistMode,
    pub max_depth: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistPlan {
    pub base_title: String,
    pub m3u_path: PathBuf,
    pub contents: String,
    pub disc_count: usize,
    pub has_duplicate_numbers: bool,
}

/// Build the set of playlists to write for a scan directory. The directory is
/// walked once via the shared recursive collector; everything else is pure.
/// No conflict resolution and no writing happen here; the caller owns those.
pub fn plan_playlists(opts: &PlaylistOptions) -> std::io::Result<Vec<PlaylistPlan>> {
    let files = collect_files_with_exts(opts.scan_dir, opts.extensions, opts.max_depth)?;
    let groups = group_disc_files(&files);

    let mut plans = Vec::new();
    for group in groups {
        if matches!(opts.mode, PlaylistMode::Multiple) && group.len() <= 1 {
            continue;
        }
        let disc_count = group.len();
        let m3u_dir = m3u_dir(opts, &group.discs);
        let m3u_path = m3u_dir.join(format!("{}.m3u", group.base_title));
        let mut contents = String::new();
        for disc in &group.discs {
            contents.push_str(&relative_entry(&m3u_dir, disc));
            contents.push('\n');
        }
        plans.push(PlaylistPlan {
            base_title: group.base_title,
            m3u_path,
            disc_count,
            has_duplicate_numbers: group.has_duplicate_numbers,
            contents,
        });
    }
    Ok(plans)
}

/// Where the .m3u lands: an explicit output directory wins; otherwise it sits
/// beside the discs when they share one parent, falling back to the scan root
/// when a set spans subdirectories.
fn m3u_dir(opts: &PlaylistOptions, discs: &[PathBuf]) -> PathBuf {
    if let Some(dir) = opts.output_dir {
        return dir.to_path_buf();
    }
    let mut parents = discs.iter().filter_map(|d| d.parent());
    if let Some(first) = parents.next()
        && parents.all(|p| p == first)
    {
        return first.to_path_buf();
    }
    opts.scan_dir.to_path_buf()
}

/// A playlist entry relative to the .m3u location, always using forward
/// slashes since emulators read .m3u paths as POSIX paths.
fn relative_entry(m3u_dir: &Path, disc: &Path) -> String {
    let rel = relativize(m3u_dir, disc);
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            Component::ParentDir => Some("..".to_string()),
            Component::CurDir => None,
            _ => Some(c.as_os_str().to_string_lossy().into_owned()),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn relativize(base: &Path, target: &Path) -> PathBuf {
    let base: Vec<Component> = base.components().collect();
    let target: Vec<Component> = target.components().collect();
    let shared = base
        .iter()
        .zip(target.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut rel = PathBuf::new();
    for _ in shared..base.len() {
        rel.push("..");
    }
    for comp in &target[shared..] {
        rel.push(comp.as_os_str());
    }
    rel
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::write(path, b"").unwrap();
    }

    fn exts() -> Vec<&'static str> {
        vec!["cue", "chd", "iso", "cso", "zso"]
    }

    fn options<'a>(
        dir: &'a Path,
        output_dir: Option<&'a Path>,
        ext: &'a [&'a str],
        mode: PlaylistMode,
    ) -> PlaylistOptions<'a> {
        PlaylistOptions {
            scan_dir: dir,
            output_dir,
            extensions: ext,
            mode,
            max_depth: None,
        }
    }

    #[test]
    fn multiple_mode_skips_single_disc() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("Sonic.cue"));
        let ext = exts();
        let plans =
            plan_playlists(&options(dir.path(), None, &ext, PlaylistMode::Multiple)).unwrap();
        assert!(plans.is_empty());
    }

    #[test]
    fn always_mode_writes_single_disc() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("Sonic.cue"));
        let ext = exts();
        let plans = plan_playlists(&options(dir.path(), None, &ext, PlaylistMode::Always)).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].contents, "Sonic.cue\n");
    }

    #[test]
    fn discs_in_order() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("Game (Disc 2).cue"));
        touch(&dir.path().join("Game (Disc 1).cue"));
        let ext = exts();
        let plans =
            plan_playlists(&options(dir.path(), None, &ext, PlaylistMode::Multiple)).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].contents, "Game (Disc 1).cue\nGame (Disc 2).cue\n");
    }

    #[test]
    fn relative_entries_are_filenames_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("Game (Disc 1).cue"));
        touch(&dir.path().join("Game (Disc 2).cue"));
        let ext = exts();
        let plans =
            plan_playlists(&options(dir.path(), None, &ext, PlaylistMode::Multiple)).unwrap();
        for line in plans[0].contents.lines() {
            assert!(!line.starts_with('/'));
            assert!(!line.contains('/'));
        }
    }

    #[test]
    fn m3u_named_after_base_title() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("Game (Disc 1).cue"));
        touch(&dir.path().join("Game (Disc 2).cue"));
        let ext = exts();
        let plans =
            plan_playlists(&options(dir.path(), None, &ext, PlaylistMode::Multiple)).unwrap();
        assert_eq!(
            plans[0].m3u_path.file_name().unwrap().to_str().unwrap(),
            "Game.m3u"
        );
    }

    #[test]
    fn forward_slash_entries_across_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        touch(&sub.join("Game (Disc 1).cue"));
        touch(&dir.path().join("Game (Disc 2).cue"));
        let ext = exts();
        let plans =
            plan_playlists(&options(dir.path(), None, &ext, PlaylistMode::Multiple)).unwrap();
        assert_eq!(plans[0].m3u_path.parent().unwrap(), dir.path());
        let lines: Vec<&str> = plans[0].contents.lines().collect();
        assert!(lines.iter().any(|l| *l == "sub/Game (Disc 1).cue"));
        assert!(lines.iter().all(|l| !l.contains('\\')));
    }

    #[test]
    fn output_dir_redirects_m3u() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        touch(&dir.path().join("Game (Disc 1).cue"));
        touch(&dir.path().join("Game (Disc 2).cue"));
        let ext = exts();
        let plans = plan_playlists(&options(
            dir.path(),
            Some(out.path()),
            &ext,
            PlaylistMode::Multiple,
        ))
        .unwrap();
        assert_eq!(plans[0].m3u_path.parent().unwrap(), out.path());
        for line in plans[0].contents.lines() {
            assert!(line.starts_with("..") || line.contains('/'));
        }
    }

    #[test]
    fn duplicate_numbers_propagated() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("Game (Disc 1).cue"));
        touch(&dir.path().join("Game (Disc 1).chd"));
        let ext = exts();
        let plans =
            plan_playlists(&options(dir.path(), None, &ext, PlaylistMode::Multiple)).unwrap();
        assert_eq!(plans.len(), 1);
        assert!(plans[0].has_duplicate_numbers);
    }
}

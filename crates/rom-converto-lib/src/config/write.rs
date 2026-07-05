//! Writing a single named preset back into a config file without disturbing
//! anything else in it. Used by the GUI settings view, which reads and
//! writes the same `[presets.<name>]` tables the CLI reads.

use std::io::Write;
use std::path::Path;

use anyhow::Context;
use toml_edit::{DocumentMut, Item, Table};

use super::Preset;

/// Adds or replaces `[presets.<name>]` in the config file at `path` with
/// `preset`, leaving every other table, key, and comment untouched. Creates
/// the file (and its parent directory) if it does not exist yet.
pub fn upsert_preset(path: &Path, name: &str, preset: &Preset) -> anyhow::Result<()> {
    let mut doc = read_doc(path)?;

    let preset_doc =
        toml_edit::ser::to_document(preset).context("cannot serialize preset to TOML")?;
    let mut preset_table = preset_doc.as_table().clone();
    // Hides an empty `[presets.name]` header when the preset only sets
    // per-format subtables, so the file reads as `[presets.name.dol]` etc.
    preset_table.set_implicit(true);

    let presets = presets_table_mut(&mut doc);
    presets.insert(name, Item::Table(preset_table));

    write_doc(path, &doc)
}

/// Removes `[presets.<name>]` from the config file at `path`, if present.
/// A missing file or a missing preset is not an error.
pub fn remove_preset(path: &Path, name: &str) -> anyhow::Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let mut doc = read_doc(path)?;
    if let Some(presets) = doc.get_mut("presets").and_then(Item::as_table_mut) {
        presets.remove(name);
    }
    write_doc(path, &doc)
}

fn read_doc(path: &Path) -> anyhow::Result<DocumentMut> {
    let content = if path.is_file() {
        std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config file: {}", path.display()))?
    } else {
        String::new()
    };
    content
        .parse::<DocumentMut>()
        .with_context(|| format!("invalid config file: {}", path.display()))
}

/// Writes `doc` via a temp file in the same directory followed by a rename,
/// so a crash or I/O error mid-write cannot leave the config file
/// truncated or partially overwritten.
fn write_doc(path: &Path, doc: &DocumentMut) -> anyhow::Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create directory: {}", parent.display()))?;

    let mut tmp = tempfile::Builder::new()
        .prefix(".rom-converto-config-")
        .suffix(".toml.tmp")
        .tempfile_in(parent)
        .with_context(|| format!("cannot create temp file in: {}", parent.display()))?;
    tmp.write_all(doc.to_string().as_bytes())
        .with_context(|| format!("cannot write config file: {}", path.display()))?;
    tmp.persist(path)
        .with_context(|| format!("cannot replace config file: {}", path.display()))?;
    Ok(())
}

/// Returns the `presets` table, creating it (as an implicit table, so it
/// renders as `[presets.<name>]` rather than a bare `[presets]` header) if
/// this is the first preset written to the file.
fn presets_table_mut(doc: &mut DocumentMut) -> &mut Table {
    if doc.get("presets").is_none() {
        let mut table = Table::new();
        table.set_implicit(true);
        doc.insert("presets", Item::Table(table));
    }
    doc["presets"]
        .as_table_mut()
        .expect("presets is always inserted as a table")
}

#[cfg(test)]
mod tests {
    use super::super::{DiscDefaults, NxDefaults, Preset, load_config};
    use super::*;

    fn write_file(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn comment_above_existing_table_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(
            dir.path(),
            "rom-converto.toml",
            "# my dol settings\n[dol]\nlevel = 5\n",
        );

        let preset = Preset {
            nx: Some(NxDefaults {
                level: Some(22),
                ..Default::default()
            }),
            ..Default::default()
        };
        upsert_preset(&path, "archive", &preset).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("# my dol settings"));
        assert!(content.contains("[dol]"));
    }

    #[test]
    fn new_preset_is_added_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "rom-converto.toml", "[dol]\nlevel = 5\n");

        let preset = Preset {
            dol: Some(DiscDefaults {
                level: Some(22),
                chunk_size: Some(131072),
                ..Default::default()
            }),
            ..Default::default()
        };
        upsert_preset(&path, "archive", &preset).unwrap();

        let cfg = load_config(Some(&path)).unwrap();
        let saved = cfg.presets.get("archive").unwrap();
        assert_eq!(saved.dol.as_ref().unwrap().level, Some(22));
        assert_eq!(saved.dol.as_ref().unwrap().chunk_size, Some(131072));
        // The pre-existing top-level default is untouched.
        assert_eq!(cfg.dol.as_ref().unwrap().level, Some(5));
    }

    #[test]
    fn existing_preset_is_replaced_not_duplicated() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(
            dir.path(),
            "rom-converto.toml",
            "[presets.archive.dol]\nlevel = 5\n",
        );

        let preset = Preset {
            dol: Some(DiscDefaults {
                level: Some(22),
                ..Default::default()
            }),
            ..Default::default()
        };
        upsert_preset(&path, "archive", &preset).unwrap();

        let cfg = load_config(Some(&path)).unwrap();
        assert_eq!(cfg.presets.len(), 1);
        assert_eq!(cfg.presets["archive"].dol.as_ref().unwrap().level, Some(22));
    }

    #[test]
    fn remove_preset_drops_only_that_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_file(
            dir.path(),
            "rom-converto.toml",
            "[presets.archive.dol]\nlevel = 5\n[presets.fast.dol]\nlevel = 1\n",
        );

        remove_preset(&path, "archive").unwrap();

        let cfg = load_config(Some(&path)).unwrap();
        assert!(!cfg.presets.contains_key("archive"));
        assert!(cfg.presets.contains_key("fast"));
    }

    #[test]
    fn remove_preset_on_missing_file_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        remove_preset(&path, "archive").unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn upsert_creates_file_and_parent_dir_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("rom-converto.toml");

        let preset = Preset {
            nx: Some(NxDefaults {
                level: Some(10),
                ..Default::default()
            }),
            ..Default::default()
        };
        upsert_preset(&path, "fast", &preset).unwrap();

        let cfg = load_config(Some(&path)).unwrap();
        assert_eq!(cfg.presets["fast"].nx.as_ref().unwrap().level, Some(10));
    }
}

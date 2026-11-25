//! `rom-converto nx extract` against synthetic NSPs. Every NCA is stored
//! plaintext (zeroed header padding at 0x340), so no real keys are needed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const APP_ID: u64 = 0x0100_AAAA_0001_0000;
const PATCH_ID: u64 = APP_ID | 0x800;
const DLC_ID: u64 = 0x0100_AAAA_0001_1001;
const APP: &str = "0100AAAA00010000";
const DLC: &str = "0100AAAA00011001";
const V1: u32 = 65536;
const V2: u32 = 131072;

const NCA_PROGRAM: u8 = 0;
const NCA_META: u8 = 1;
const NCA_DATA: u8 = 4;
const CNMT_APPLICATION: u8 = 0x80;
const CNMT_PATCH: u8 = 0x81;
const CNMT_ADD_ON_CONTENT: u8 = 0x82;
const CNMT_PROGRAM: u8 = 1;
const CNMT_DATA: u8 = 2;
const FS_ROMFS: u8 = 0;
const FS_PARTITION: u8 = 1;
const HASH_SHA256: u8 = 2;
const HASH_IVFC: u8 = 3;
const ENC_AES_CTR: u8 = 3;
const ENC_AES_CTR_EX: u8 = 4;
/// Section offset of the IVFC data level (the RomFS image).
const DATA_LEVEL: u64 = 0x4000;
const EMPTY: u32 = u32::MAX;

type Entry = (String, Vec<u8>);
type FsHeader = [u8; 0x200];

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

fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn pad_to(buf: &mut Vec<u8>, align: usize) {
    buf.resize(buf.len().next_multiple_of(align), 0);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Deterministic xorshift bytes, so a read from a wrong offset never
/// matches by accident.
fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn pfs0(files: &[Entry]) -> Vec<u8> {
    let mut entries = Vec::new();
    let mut strings = Vec::new();
    let mut data_offset = 0u64;
    for (name, data) in files {
        entries.extend_from_slice(&data_offset.to_le_bytes());
        entries.extend_from_slice(&(data.len() as u64).to_le_bytes());
        entries.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        entries.extend_from_slice(&0u32.to_le_bytes());
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        data_offset += data.len() as u64;
    }
    pad_to(&mut strings, 0x10);
    let mut out = b"PFS0".to_vec();
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    out.extend_from_slice(&(strings.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&entries);
    out.extend_from_slice(&strings);
    for (_, data) in files {
        out.extend_from_slice(data);
    }
    out
}

/// RomFS image of `files` (`/`-separated paths, file data laid out in
/// list order). Returns the image and each file's data offset in it.
fn romfs(files: &[Entry]) -> (Vec<u8>, Vec<u64>) {
    struct Dir {
        name: String,
        parent: usize,
        dirs: Vec<usize>,
        files: Vec<usize>,
    }
    let mut dirs = vec![Dir {
        name: String::new(),
        parent: 0,
        dirs: Vec::new(),
        files: Vec::new(),
    }];
    let mut file_dirs = Vec::new();
    let mut file_names = Vec::new();
    for (path, _) in files {
        let mut parts: Vec<&str> = path.split('/').collect();
        let name = parts.pop().expect("split yields a file name");
        let mut cur = 0;
        for part in parts {
            let found = dirs[cur]
                .dirs
                .iter()
                .copied()
                .find(|&d| dirs[d].name == part);
            cur = found.unwrap_or_else(|| {
                dirs.push(Dir {
                    name: part.into(),
                    parent: cur,
                    dirs: Vec::new(),
                    files: Vec::new(),
                });
                let id = dirs.len() - 1;
                dirs[cur].dirs.push(id);
                id
            });
        }
        dirs[cur].files.push(file_names.len());
        file_dirs.push(cur);
        file_names.push(name);
    }

    let mut dir_offs = Vec::new();
    let mut at = 0;
    for d in &dirs {
        dir_offs.push(at as u32);
        at += 0x18 + d.name.len().next_multiple_of(4);
    }
    let mut file_offs = Vec::new();
    at = 0;
    for name in &file_names {
        file_offs.push(at as u32);
        at += 0x20 + name.len().next_multiple_of(4);
    }
    let next = |list: &[usize], item: usize, offs: &[u32]| {
        list.iter()
            .position(|&x| x == item)
            .and_then(|p| list.get(p + 1))
            .map_or(EMPTY, |&x| offs[x])
    };

    // One hash bucket per table: every entry chains from offset 0
    // through hash_next in table order.
    let mut dir_table = Vec::new();
    for (i, d) in dirs.iter().enumerate() {
        for v in [
            dir_offs[d.parent],
            next(&dirs[d.parent].dirs, i, &dir_offs),
            d.dirs.first().map_or(EMPTY, |&c| dir_offs[c]),
            d.files.first().map_or(EMPTY, |&f| file_offs[f]),
            dir_offs.get(i + 1).copied().unwrap_or(EMPTY),
            d.name.len() as u32,
        ] {
            dir_table.extend_from_slice(&v.to_le_bytes());
        }
        dir_table.extend_from_slice(d.name.as_bytes());
        pad_to(&mut dir_table, 4);
    }

    let mut data = Vec::new();
    let mut data_offs = Vec::new();
    for (_, bytes) in files {
        pad_to(&mut data, 0x10);
        data_offs.push(data.len() as u64);
        data.extend_from_slice(bytes);
    }
    let mut file_table = Vec::new();
    for (i, name) in file_names.iter().enumerate() {
        let parent = file_dirs[i];
        file_table.extend_from_slice(&dir_offs[parent].to_le_bytes());
        file_table.extend_from_slice(&next(&dirs[parent].files, i, &file_offs).to_le_bytes());
        file_table.extend_from_slice(&data_offs[i].to_le_bytes());
        file_table.extend_from_slice(&(files[i].1.len() as u64).to_le_bytes());
        let hash_next = file_offs.get(i + 1).copied().unwrap_or(EMPTY);
        file_table.extend_from_slice(&hash_next.to_le_bytes());
        file_table.extend_from_slice(&(name.len() as u32).to_le_bytes());
        file_table.extend_from_slice(name.as_bytes());
        pad_to(&mut file_table, 4);
    }

    const FILE_DATA: u64 = 0x200;
    let mut image = vec![0u8; FILE_DATA as usize];
    image.extend_from_slice(&data);
    pad_to(&mut image, 4);
    let mut append = |bytes: &[u8]| {
        let off = image.len() as u64;
        image.extend_from_slice(bytes);
        [off, bytes.len() as u64]
    };
    let dir_hash = append(&0u32.to_le_bytes());
    let dir_meta = append(&dir_table);
    let file_hash = append(&0u32.to_le_bytes());
    let file_meta = append(&file_table);
    let header = [
        0x50,
        dir_hash[0],
        dir_hash[1],
        dir_meta[0],
        dir_meta[1],
        file_hash[0],
        file_hash[1],
        file_meta[0],
        file_meta[1],
        FILE_DATA,
    ];
    for (i, v) in header.into_iter().enumerate() {
        put_u64(&mut image, i * 8, v);
    }
    (image, data_offs.iter().map(|o| FILE_DATA + o).collect())
}

fn fs_header(fs_type: u8, hash_type: u8) -> FsHeader {
    let mut fs = [0u8; 0x200];
    fs[0] = 2;
    fs[2] = fs_type;
    fs[3] = hash_type;
    fs[4] = ENC_AES_CTR;
    fs
}

/// PFS0 section behind a HierarchicalSha256 hash table (ExeFS and meta NCAs).
fn pfs0_section(files: &[Entry]) -> (Vec<u8>, FsHeader) {
    let pfs0 = pfs0(files);
    let mut data = vec![0u8; 0x200];
    data.extend_from_slice(&pfs0);
    pad_to(&mut data, 0x200);
    let mut fs = fs_header(FS_PARTITION, HASH_SHA256);
    put_u32(&mut fs, 0x28, 0x1000);
    put_u32(&mut fs, 0x2C, 2);
    put_u64(&mut fs, 0x38, 0x20);
    put_u64(&mut fs, 0x40, 0x200);
    put_u64(&mut fs, 0x48, pfs0.len() as u64);
    (data, fs)
}

/// RomFS section with IVFC levels 0..4 as (unchecked) hash levels and
/// level 5 holding `image` at [`DATA_LEVEL`].
fn romfs_section(image: &[u8]) -> (Vec<u8>, FsHeader) {
    let mut data = vec![0u8; DATA_LEVEL as usize];
    data.extend_from_slice(image);
    pad_to(&mut data, 0x200);
    let mut fs = fs_header(FS_ROMFS, HASH_IVFC);
    fs[8..12].copy_from_slice(b"IVFC");
    put_u32(&mut fs, 0x0C, 0x2_0000);
    put_u32(&mut fs, 0x10, 0x20);
    put_u32(&mut fs, 0x14, 7);
    for level in 0..6 {
        let at = 0x18 + level * 0x18;
        let (offset, size) = if level == 5 {
            (DATA_LEVEL, image.len() as u64)
        } else {
            (level as u64 * 0x200, 0x20)
        };
        put_u64(&mut fs, at, offset);
        put_u64(&mut fs, at + 8, size);
        put_u32(&mut fs, at + 0x10, 0xE);
    }
    (data, fs)
}

/// Bucket tree table with one entry set: the L1 node, then the entry-set node.
fn bucket_tree(entries: &[u8], count: u32, end: u64) -> Vec<u8> {
    let mut table = vec![0u8; 0x8000];
    put_u32(&mut table, 4, 1);
    put_u64(&mut table, 8, end);
    put_u32(&mut table, 0x4004, count);
    put_u64(&mut table, 0x4008, end);
    table[0x4010..0x4010 + entries.len()].copy_from_slice(entries);
    table
}

/// Writes a PatchInfo table slot: offset, size, then its `BKTR` header.
fn put_table(fs: &mut FsHeader, at: usize, offset: u64, size: u64, entry_count: u32) {
    put_u64(fs, at, offset);
    put_u64(fs, at + 8, size);
    fs[at + 0x10..at + 0x14].copy_from_slice(b"BKTR");
    put_u32(fs, at + 0x14, 1);
    put_u32(fs, at + 0x18, entry_count);
}

/// Plaintext NCA3: the key area and the padding at 0x340 stay zero.
fn nca(content_type: u8, title_id: u64, sections: &[(Vec<u8>, FsHeader)]) -> Vec<u8> {
    let mut nca = vec![0u8; 0x4000];
    for (i, (data, fs)) in sections.iter().enumerate() {
        let start = nca.len() as u32 / 0x200;
        nca.extend_from_slice(data);
        pad_to(&mut nca, 0x200);
        let end = nca.len() as u32 / 0x200;
        put_u32(&mut nca, 0x240 + i * 0x10, start);
        put_u32(&mut nca, 0x244 + i * 0x10, end);
        nca[0x400 + i * 0x200..][..0x200].copy_from_slice(fs);
    }
    nca[0x200..0x204].copy_from_slice(b"NCA3");
    nca[0x205] = content_type;
    let size = nca.len() as u64;
    put_u64(&mut nca, 0x208, size);
    put_u64(&mut nca, 0x210, title_id);
    nca
}

/// NSP members of one title: its meta NCA, whose CNMT lists `content`,
/// and `content` itself. `seed` keeps content ids unique per title.
fn title(meta_type: u8, title_id: u64, version: u32, content: Vec<u8>, seed: u8) -> Vec<Entry> {
    let content_id = [seed; 16];
    let meta_id = [seed + 0x80; 16];
    // Patch and AddOnContent extended headers start with the base
    // application id, an Application's with its patch id.
    let (link, ext_len, record_type) = match meta_type {
        CNMT_APPLICATION => (PATCH_ID, 0x10, CNMT_PROGRAM),
        CNMT_PATCH => (APP_ID, 0x18, CNMT_PROGRAM),
        _ => (APP_ID, 0x10, CNMT_DATA),
    };
    let mut cnmt = Vec::new();
    cnmt.extend_from_slice(&title_id.to_le_bytes());
    cnmt.extend_from_slice(&version.to_le_bytes());
    cnmt.extend_from_slice(&[meta_type, 0]);
    cnmt.extend_from_slice(&(ext_len as u16).to_le_bytes());
    cnmt.extend_from_slice(&1u16.to_le_bytes());
    cnmt.resize(0x20, 0);
    cnmt.extend_from_slice(&link.to_le_bytes());
    cnmt.resize(0x20 + ext_len, 0);
    cnmt.extend_from_slice(&[0u8; 0x20]);
    cnmt.extend_from_slice(&content_id);
    cnmt.extend_from_slice(&(content.len() as u64).to_le_bytes()[..6]);
    cnmt.extend_from_slice(&[record_type, 0]);
    let meta = nca(
        NCA_META,
        title_id,
        &[pfs0_section(&[(format!("{title_id:016x}.cnmt"), cnmt)])],
    );
    vec![
        (format!("{}.cnmt.nca", hex(&meta_id)), meta),
        (format!("{}.nca", hex(&content_id)), content),
    ]
}

fn base_exefs() -> Vec<Entry> {
    vec![
        ("main".into(), noise(0x400, 10)),
        ("main.npdm".into(), noise(0x80, 11)),
    ]
}

fn base_romfs() -> Vec<Entry> {
    vec![
        ("a.bin".into(), noise(0x3000, 1)),
        ("dir/sub/b.txt".into(), b"base text\n".to_vec()),
        ("dir/c.dat".into(), noise(0x1234, 2)),
    ]
}

fn base_title(romfs_files: &[Entry]) -> Vec<Entry> {
    let (image, _) = romfs(romfs_files);
    let program = nca(
        NCA_PROGRAM,
        APP_ID,
        &[pfs0_section(&base_exefs()), romfs_section(&image)],
    );
    title(CNMT_APPLICATION, APP_ID, 0, program, 1)
}

fn update_exefs(version: u32) -> Vec<Entry> {
    let n = version >> 16;
    vec![
        ("main".into(), noise(0x500, 20 + n)),
        ("main.npdm".into(), noise(0x90, 30 + n)),
    ]
}

/// The RomFS a patch of `version` produces: `dir/c.dat` changed, `a.bin`
/// and `dir/sub/b.txt` unchanged, `new/d.bin` added. The changed file
/// comes first so `a.bin` sits at a different offset than in the base.
fn patched_romfs(version: u32) -> Vec<Entry> {
    let n = version >> 16;
    let base = base_romfs();
    vec![
        ("dir/c.dat".into(), noise(0x1500, 40 + n)),
        base[0].clone(),
        base[1].clone(),
        ("new/d.bin".into(), noise(0x800, 50 + n)),
    ]
}

/// Patch RomFS section: [patch data][indirect table][AesCtrEx table], its
/// IVFC levels describing the patched virtual space. The virtual range
/// of `a.bin` maps to the base RomFS section (storage 0) and is left out
/// of the patch data; everything else comes from the patch data
/// (storage 1).
fn patch_romfs_section(version: u32) -> (Vec<u8>, FsHeader) {
    let base_a = DATA_LEVEL + romfs(&base_romfs()).1[0];
    let (image, offsets) = romfs(&patched_romfs(version));
    let (virtual_section, mut fs) = romfs_section(&image);
    let start = DATA_LEVEL + offsets[1];
    let end = start + base_romfs()[0].1.len() as u64;

    let mut indirect = Vec::new();
    for (virt, phys, storage) in [(0, 0, 1u32), (start, base_a, 0), (end, start, 1)] {
        indirect.extend_from_slice(&virt.to_le_bytes());
        indirect.extend_from_slice(&phys.to_le_bytes());
        indirect.extend_from_slice(&storage.to_le_bytes());
    }
    let mut section = virtual_section[..start as usize].to_vec();
    section.extend_from_slice(&virtual_section[end as usize..]);
    pad_to(&mut section, 0x200);
    let indirect_offset = section.len() as u64;
    section.extend(bucket_tree(&indirect, 3, virtual_section.len() as u64));
    let aes_ctr_ex_offset = section.len() as u64;
    let mut aes_ctr_ex = [0u8; 0x10];
    put_u32(&mut aes_ctr_ex, 0x0C, 1);
    section.extend(bucket_tree(&aes_ctr_ex, 1, aes_ctr_ex_offset));

    fs[4] = ENC_AES_CTR_EX;
    put_table(&mut fs, 0x100, indirect_offset, 0x8000, 3);
    put_table(&mut fs, 0x120, aes_ctr_ex_offset, 0x8000, 1);
    (section, fs)
}

fn update_title(version: u32) -> Vec<Entry> {
    let program = nca(
        NCA_PROGRAM,
        PATCH_ID,
        &[
            pfs0_section(&update_exefs(version)),
            patch_romfs_section(version),
        ],
    );
    title(
        CNMT_PATCH,
        PATCH_ID,
        version,
        program,
        0x10 + (version >> 16) as u8,
    )
}

fn dlc_romfs() -> Vec<Entry> {
    vec![
        ("item.bin".into(), noise(0x900, 60)),
        ("data/level.dat".into(), noise(0x40, 61)),
    ]
}

fn dlc_title() -> Vec<Entry> {
    let (image, _) = romfs(&dlc_romfs());
    let data = nca(NCA_DATA, DLC_ID, &[romfs_section(&image)]);
    title(CNMT_ADD_ON_CONTENT, DLC_ID, 0, data, 4)
}

/// Every file under `root`, keyed by its `/`-separated relative path.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let rel = path.strip_prefix(root).expect("path under root");
                let rel = rel.to_string_lossy().replace('\\', "/");
                files.insert(rel, fs::read(&path).expect("read output file"));
            }
        }
    }
    files
}

fn names(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .expect("read dir")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The tree `<title>/<part>/<path>` expected under the output dir.
fn expected(parts: &[(&str, &str, Vec<Entry>)]) -> BTreeMap<String, Vec<u8>> {
    parts
        .iter()
        .flat_map(|(title, part, files)| {
            files
                .iter()
                .map(move |(path, data)| (format!("{title}/{part}/{path}"), data.clone()))
        })
        .collect()
}

struct Env {
    dir: tempfile::TempDir,
    keys: PathBuf,
    out: PathBuf,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let keys = dir.path().join("prod.keys");
        fs::write(&keys, format!("header_key = {}\n", "00".repeat(32))).expect("write keys");
        let out = dir.path().join("out");
        Self { dir, keys, out }
    }

    fn input(&self, name: &str, members: Vec<Entry>) -> PathBuf {
        let path = self.dir.path().join(name);
        fs::write(&path, pfs0(&members)).expect("write input");
        path
    }

    fn extract(&self, inputs: &[&Path], flags: &[&str]) -> Output {
        bin()
            .current_dir(self.dir.path())
            .args(["nx", "extract"])
            .args(inputs)
            .arg("--output-dir")
            .arg(&self.out)
            .arg("--keys")
            .arg(&self.keys)
            .args(flags)
            .output()
            .expect("run rom-converto")
    }
}

#[test]
fn base_game_extracts_exefs_and_romfs_under_its_title_id() {
    let env = Env::new();
    let base = env.input("base.nsp", base_title(&base_romfs()));

    let output = env.extract(&[&base], &[]);

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        tree(&env.out),
        expected(&[(APP, "exefs", base_exefs()), (APP, "romfs", base_romfs())])
    );
}

#[test]
fn part_flags_select_romfs_exefs_or_both() {
    let cases: [(&[&str], &[&str]); 3] = [
        (&["--romfs"], &["romfs"]),
        (&["--exefs"], &["exefs"]),
        (&["--romfs", "--exefs"], &["exefs", "romfs"]),
    ];
    for (flags, parts) in cases {
        let env = Env::new();
        let base = env.input("base.nsp", base_title(&base_romfs()));

        let output = env.extract(&[&base], flags);

        assert!(output.status.success(), "{flags:?}: {}", combined(&output));
        let want: Vec<_> = parts
            .iter()
            .map(|&part| {
                let files = if part == "exefs" {
                    base_exefs()
                } else {
                    base_romfs()
                };
                (APP, part, files)
            })
            .collect();
        assert_eq!(tree(&env.out), expected(&want), "{flags:?}");
        assert_eq!(names(&env.out.join(APP)), set(parts), "{flags:?}");
    }
}

#[test]
fn update_in_separate_file_patches_romfs_and_supplies_exefs() {
    let env = Env::new();
    let base = env.input("base.nsp", base_title(&base_romfs()));
    let update = env.input("update.nsp", update_title(V1));

    let output = env.extract(&[&base, &update], &[]);

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        tree(&env.out),
        expected(&[
            (APP, "exefs", update_exefs(V1)),
            (APP, "romfs", patched_romfs(V1)),
        ])
    );
}

#[test]
fn merged_container_extracts_patched_game_and_dlc_romfs() {
    let env = Env::new();
    let mut members = base_title(&base_romfs());
    members.extend(update_title(V1));
    members.extend(dlc_title());
    let merged = env.input("merged.nsp", members);

    let output = env.extract(&[&merged], &[]);

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        tree(&env.out),
        expected(&[
            (APP, "exefs", update_exefs(V1)),
            (APP, "romfs", patched_romfs(V1)),
            (DLC, "romfs", dlc_romfs()),
        ])
    );
    assert_eq!(names(&env.out.join(DLC)), set(&["romfs"]));
}

#[test]
fn highest_update_version_wins() {
    for order in [[V1, V2], [V2, V1]] {
        let env = Env::new();
        let base = env.input("base.nsp", base_title(&base_romfs()));
        // Names follow argument order, so sorting inputs by path cannot
        // undo the order under test.
        let updates: Vec<PathBuf> = order
            .iter()
            .enumerate()
            .map(|(i, &v)| env.input(&format!("update-{i}.nsp"), update_title(v)))
            .collect();

        let output = env.extract(&[&base, &updates[0], &updates[1]], &[]);

        assert!(output.status.success(), "{order:?}: {}", combined(&output));
        assert_eq!(
            tree(&env.out),
            expected(&[
                (APP, "exefs", update_exefs(V2)),
                (APP, "romfs", patched_romfs(V2)),
            ]),
            "{order:?}"
        );
    }
}

#[test]
fn lone_update_extracts_exefs_and_warns_about_romfs() {
    let env = Env::new();
    let update = env.input("update.nsp", update_title(V1));

    let output = env.extract(&[&update], &[]);

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        tree(&env.out),
        expected(&[(APP, "exefs", update_exefs(V1))])
    );
    assert_eq!(names(&env.out), set(&[APP]));
    assert_eq!(names(&env.out.join(APP)), set(&["exefs"]));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(&format!("{PATCH_ID:016X}")), "{stderr}");
    assert!(stderr.contains("needs the base game"), "{stderr}");
}

#[test]
fn dry_run_writes_nothing() {
    let env = Env::new();
    let base = env.input("base.nsp", base_title(&base_romfs()));
    let update = env.input("update.nsp", update_title(V1));

    let output = env.extract(&[&base, &update], &["--dry-run"]);

    assert!(output.status.success(), "{}", combined(&output));
    assert!(tree(&env.out).is_empty(), "{:?}", tree(&env.out).keys());
}

#[test]
fn existing_target_is_kept_unless_forced() {
    let env = Env::new();
    let base = env.input("base.nsp", base_title(&base_romfs()));
    let update = env.input("update.nsp", update_title(V1));
    let first = env.extract(&[&base], &[]);
    assert!(first.status.success(), "{}", combined(&first));
    fs::write(env.out.join(APP).join("romfs").join("stale.txt"), b"stale").unwrap();
    let before = tree(&env.out);

    let refused = env.extract(&[&base, &update], &[]);

    assert!(!refused.status.success(), "{}", combined(&refused));
    assert_eq!(tree(&env.out), before);

    let skipped = env.extract(&[&base, &update], &["--on-conflict", "skip"]);

    assert!(skipped.status.success(), "{}", combined(&skipped));
    assert_eq!(tree(&env.out), before);

    let forced = env.extract(&[&base, &update], &["-f"]);

    assert!(forced.status.success(), "{}", combined(&forced));
    assert_eq!(
        tree(&env.out),
        expected(&[
            (APP, "exefs", update_exefs(V1)),
            (APP, "romfs", patched_romfs(V1)),
        ])
    );
}

#[test]
fn romfs_entry_named_dot_dot_fails_without_escaping_output() {
    let mut env = Env::new();
    let mut romfs_files = base_romfs();
    // Deep enough that a write through the target or a scratch dir next
    // to it would land outside the output dir but inside the tempdir.
    romfs_files.push(("../../../../../escape.txt".into(), b"escaped".to_vec()));
    let evil = env.input("evil.nsp", base_title(&romfs_files));

    // The rest of the input is valid: its ExeFS alone extracts.
    env.out = env.dir.path().join("exefs-only");
    let control = env.extract(&[&evil], &["--exefs"]);
    assert!(control.status.success(), "{}", combined(&control));

    env.out = env.dir.path().join("a/b/c/out");
    fs::create_dir_all(&env.out).unwrap();
    let output = env.extract(&[&evil], &[]);

    assert!(!output.status.success(), "{}", combined(&output));
    let outside: Vec<String> = tree(env.dir.path())
        .into_keys()
        .filter(|p| !p.starts_with("a/b/c/out/") && !p.starts_with("exefs-only/"))
        .collect();
    assert_eq!(outside, ["evil.nsp", "prod.keys"]);
}

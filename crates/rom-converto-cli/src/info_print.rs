use anyhow::Result;
use rom_converto_lib::info::{DiscContent, InfoResult};
use rom_converto_lib::microsoft::xex::XexInfo;
use std::fmt::Write;

pub struct KeyValueTable {
    rows: Vec<(String, String)>,
    longest_key: usize,
}

impl KeyValueTable {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            longest_key: 0,
        }
    }

    pub fn push<K: Into<String>, V: Into<String>>(&mut self, key: K, value: V) -> &mut Self {
        let k = key.into();
        let v = value.into();
        if k.len() > self.longest_key {
            self.longest_key = k.len();
        }
        self.rows.push((k, v));
        self
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for (k, v) in &self.rows {
            let _ = writeln!(
                &mut out,
                "{:<width$}  {}",
                format!("{}:", k),
                v,
                width = self.longest_key + 1
            );
        }
        out
    }
}

impl Default for KeyValueTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Leading field order shared by every ROM section; keys outside it keep
/// the order the renderer pushed them in.
const ROM_KEY_ORDER: [&str; 6] = [
    "Title",
    "Title ID",
    "Content type",
    "Version",
    "Region",
    "Size",
];

fn order_rom(t: &mut KeyValueTable) {
    t.rows.sort_by_key(|(k, _)| {
        ROM_KEY_ORDER
            .iter()
            .position(|c| c == k)
            .unwrap_or(ROM_KEY_ORDER.len())
    });
}

fn section(out: &mut String, name: &str, t: &KeyValueTable) {
    if t.rows.is_empty() {
        return;
    }
    nested(out, name, &t.render());
}

fn nested(out: &mut String, name: &str, body: &str) {
    out.push_str(&format!("{}:\n", name));
    for line in body.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(&format!("  {}\n", line));
        }
    }
    out.push('\n');
}

fn format_maker(code: &str, name: Option<&str>) -> String {
    match name {
        Some(n) if !n.is_empty() => format!("{} ({})", code, n),
        _ => code.to_string(),
    }
}

pub fn print(result: &InfoResult, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
        return Ok(());
    }
    let rendered = match result {
        InfoResult::Chd(info) => render_chd(info),
        InfoResult::Cso(info) => render_cso(info),
        InfoResult::Ctr(info) => render_ctr(info),
        InfoResult::Dol(info) => render_dol(info),
        InfoResult::Rvl(info) => render_rvl(info),
        InfoResult::Wup(info) => render_wup(info),
        InfoResult::Nx(info) => render_nx(info),
        InfoResult::Xbox(info) => render_xbox(info),
        InfoResult::Xenon(info) => render_xenon(info),
        InfoResult::Ps3(info) => render_ps3(info),
        InfoResult::Psx(info) => render_psx(info),
        InfoResult::Psp(info) => render_psp(info),
        InfoResult::LaserDisc(info) => render_laserdisc(info),
        InfoResult::Nds(info) => render_nds(info),
        InfoResult::Retro(info) => render_retro(info),
        InfoResult::Pbp(info) => render_pbp(info),
        InfoResult::Vpk(info) => render_vpk(info),
        InfoResult::Pkg(info) => render_pkg(info),
    };
    print!("{}", rendered);
    Ok(())
}

fn render_cso(info: &rom_converto_lib::info::CsoInfo) -> String {
    let mut c = KeyValueTable::new();
    c.push("Format", format!("{} v{}", info.format, info.version));
    c.push("Block size", format!("{} bytes", info.block_size));
    c.push("Index shift", format!("{}", info.index_shift));
    c.push(
        "Blocks",
        format!("{} ({} stored raw)", info.block_count, info.raw_block_count),
    );
    c.push("Uncompressed bytes", format!("{}", info.uncompressed_size));
    c.push("Physical bytes", format!("{}", info.physical_bytes));
    c.push(
        "Compression ratio",
        format!("{:.2}%", info.compression_ratio),
    );

    let mut out = String::new();
    section(&mut out, "Container", &c);
    section(&mut out, "ROM", &disc_rom_table(info.content.as_ref()));
    out
}

fn render_chd(info: &rom_converto_lib::info::ChdInfo) -> String {
    let mut c = KeyValueTable::new();
    c.push("Format", format!("CHD v{}", info.version));
    if info.compressors.is_empty() {
        c.push("Compressors", "(none)");
    } else {
        c.push("Compressors", info.compressors.join(", "));
    }
    c.push("Hunk size", format!("{} bytes", info.hunk_bytes));
    c.push("Unit size", format!("{} bytes", info.unit_bytes));
    c.push("Hunks", format!("{}", info.hunk_count));
    c.push("Logical bytes", format!("{}", info.logical_bytes));
    c.push("Physical bytes", format!("{}", info.physical_bytes));
    c.push(
        "Compression ratio",
        format!("{:.2}%", info.compression_ratio),
    );
    c.push("Raw SHA1", info.raw_sha1.clone());
    c.push("SHA1", info.sha1.clone());
    if let Some(parent) = &info.parent_sha1 {
        c.push("Parent SHA1", parent.clone());
    }
    if let Some(vers) = &info.version_string {
        c.push("chdman version", vers.clone());
    }
    if let Some(dvd) = &info.dvd {
        let layer = match dvd.layer_class {
            rom_converto_lib::chd::info::DvdLayerClass::SingleLayer => "single-layer (4.7 GB)",
            rom_converto_lib::chd::info::DvdLayerClass::DualLayer => "dual-layer (8.5 GB)",
        };
        c.push(
            "DVD geometry",
            format!("{} sectors, {}", dvd.total_sectors, layer),
        );
    }

    let mut out = String::new();
    section(&mut out, "Container", &c);
    section(&mut out, "ROM", &disc_rom_table(info.content.as_ref()));

    if let Some(ld) = &info.ld {
        out.push_str("\nLaserDisc:\n");
        let mut l = KeyValueTable::new();
        l.push("FPS", ld.fps.clone());
        l.push("Field size", format!("{}x{}", ld.width, ld.height));
        l.push("Interlaced", if ld.interlaced { "yes" } else { "no" });
        l.push(
            "Audio",
            format!("{} ch, {} Hz", ld.channels, ld.sample_rate),
        );
        l.push("Frames", format!("{}", ld.frame_count));
        out.push_str(&l.render());

        if let Some(vbi) = &ld.vbi {
            use rom_converto_lib::chd::info::LdDiscType;
            out.push_str("\nVBI:\n");
            let mut v = KeyValueTable::new();
            v.push(
                "Disc type",
                match vbi.disc_type {
                    LdDiscType::Cav => "CAV",
                    LdDiscType::Clv => "CLV",
                    LdDiscType::Unknown => "unknown",
                },
            );
            if let (Some(min), Some(max)) = (vbi.cav_picture_min, vbi.cav_picture_max) {
                v.push("CAV picture range", format!("{}-{}", min, max));
            }
            if let (Some(start), Some(end)) = (&vbi.clv_start_time, &vbi.clv_end_time) {
                v.push(
                    "CLV time range",
                    format!(
                        "{}:{:02}-{}:{:02}",
                        start.hours, start.minutes, end.hours, end.minutes
                    ),
                );
            }
            if let (Some(min), Some(max)) = (vbi.chapter_min, vbi.chapter_max) {
                v.push("Chapters", format!("{}-{}", min, max));
            }
            v.push("White flags", format!("{}", vbi.white_flag_count));
            v.push(
                "Lead-in / lead-out",
                format!("{} / {}", vbi.lead_in, vbi.lead_out),
            );
            out.push_str(&v.render());
        }
    }

    if !info.tracks.is_empty() {
        let mut inner = String::from("Tracks:\n");
        for tr in &info.tracks {
            let postgap = tr
                .postgap
                .map(|p| format!(" postgap={}", p))
                .unwrap_or_default();
            let subtype = tr
                .subtype
                .as_deref()
                .map(|s| format!(" subcode={}", s))
                .unwrap_or_default();
            let pgtype = tr
                .pgtype
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| format!(" pgtype={}", s))
                .unwrap_or_default();
            let pgsub = tr
                .pgsub
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| format!(" pgsub={}", s))
                .unwrap_or_default();
            inner.push_str(&format!(
                "  {:>2}  {:<12}  frames={:<8} pregap={}{}{}{}{}\n",
                tr.number, tr.track_type, tr.frames, tr.pregap, pgtype, pgsub, subtype, postgap
            ));
        }
        nested(&mut out, "Inner files", &inner);
    }

    if !info.metadata_tags.is_empty() {
        out.push_str("Metadata tags:\n");
        for tag in &info.metadata_tags {
            out.push_str(&format!("  {}  ({} bytes)\n", tag.tag, tag.length));
        }
        out.push('\n');
    }

    out
}

/// ROM table for the PlayStation-family disc a CHD or CSO carries.
fn disc_rom_table(content: Option<&DiscContent>) -> KeyValueTable {
    match content {
        Some(DiscContent::Psx(psx)) => psx_rom_table(psx),
        Some(DiscContent::Psp(psp)) => psp_rom_table(psp),
        None => KeyValueTable::new(),
    }
}

fn render_ctr(info: &rom_converto_lib::info::CtrInfo) -> String {
    use rom_converto_lib::nintendo::ctr::info::CtrFormat;
    let fmt = match info.format {
        CtrFormat::Cia => "3DS CIA",
        CtrFormat::Ncsd => "3DS NCSD/CCI",
        CtrFormat::Ncch => "3DS NCCH/CXI",
        CtrFormat::Unknown => "3DS",
    };

    let mut t = KeyValueTable::new();
    t.push("Format", fmt);
    t.push("Title ID", info.title_id.clone());
    t.push("Content type", ctr_content_type(&info.title_id, fmt));
    t.push("Program ID", info.program_id.clone());
    t.push("Product code", info.product_code.clone());
    t.push(
        "Maker code",
        format_maker(&info.maker_code, info.maker_name.as_deref()),
    );
    if let Some(s) = &info.smdh
        && !s.region_names.is_empty()
    {
        t.push("Region", s.region_names.join(", "));
    }
    if let Some(sz) = info.cartridge_size {
        t.push("Cartridge size", format!("{} bytes", sz));
    }
    t.push("Physical bytes", format!("{}", info.physical_bytes));
    t.push(
        "NCCH encrypted",
        if info.ncch_encrypted { "yes" } else { "no" },
    );
    if info.compressed {
        t.push("Compressed", "yes");
    }
    if info.seed_crypto {
        t.push("Seed crypto", "yes");
        t.push(
            "Seed (local seeddb)",
            if info.seed_found == Some(true) {
                "found & verified"
            } else {
                "not found"
            },
        );
        if let Some(keyy) = &info.seed_keyy {
            t.push("Derived KeyY", keyy.clone());
        }
    }
    order_rom(&mut t);

    let mut out = String::new();
    section(&mut out, "ROM", &t);

    if let Some(s) = &info.smdh {
        out.push_str(&format!("Flags: 0x{:08X}\n\n", s.flags));

        if !s.titles.is_empty() {
            out.push_str("Titles:\n");
            for t in &s.titles {
                out.push_str(&format!(
                    "  {:<22}  {} ({})\n",
                    t.language,
                    t.long_description.replace('\n', " "),
                    t.publisher
                ));
            }
            out.push('\n');
        }
        if !s.age_ratings.is_empty() {
            out.push_str("Age ratings:\n");
            for r in &s.age_ratings {
                let banned = if r.banned { " banned" } else { "" };
                let pending = if r.pending { " pending" } else { "" };
                out.push_str(&format!(
                    "  {:<10}  age {}{}{}\n",
                    r.region, r.age, banned, pending
                ));
            }
            out.push('\n');
        }
    }

    let mut inner = String::new();
    if !info.ncsd_partitions.is_empty() {
        inner.push_str("Partitions:\n");
        for p in &info.ncsd_partitions {
            inner.push_str(&format!(
                "  {}  {}  {} bytes @ 0x{:X}\n",
                p.index, p.name, p.size, p.offset
            ));
        }
    }
    if !info.cia_contents.is_empty() {
        if !inner.is_empty() {
            inner.push('\n');
        }
        inner.push_str("Contents:\n");
        for c in &info.cia_contents {
            let encrypted = if c.encrypted { "  encrypted" } else { "" };
            inner.push_str(&format!(
                "  {}  {}  {} bytes{}\n",
                c.index, c.content_id, c.size, encrypted
            ));
        }
    }
    if !inner.is_empty() {
        nested(&mut out, "Inner files", &inner);
    }

    if let Some(img) = &info.icon {
        out.push_str(&format!(
            "Icon: {}x{} PNG ({} bytes)\n\n",
            img.width,
            img.height,
            img.png_bytes.len()
        ));
    }

    out
}

/// 3DS content class from the title-id high word, falling back to the
/// container format when the high word names no known class.
fn ctr_content_type(title_id: &str, format: &str) -> String {
    match title_id.get(..8) {
        Some("00040000") => "Game".to_string(),
        Some("0004000E") => "Update".to_string(),
        Some("0004008C") => "DLC".to_string(),
        Some("00040010") | Some("00040030") => "System".to_string(),
        _ => format.to_uppercase(),
    }
}

fn render_dol(info: &rom_converto_lib::info::DolInfo) -> String {
    let mut t = KeyValueTable::new();
    t.push("Format", format!("GameCube ({})", info.container));
    t.push("Content type", "Game");
    t.push("Game ID", info.game_id.clone());
    t.push(
        "Maker code",
        format_maker(&info.maker_code, info.maker_name.as_deref()),
    );
    t.push("Disc number", format!("{}", info.disc_number));
    t.push("Disc version", format!("{}", info.disc_version));
    t.push(
        "Audio streaming",
        if info.audio_streaming { "yes" } else { "no" },
    );
    t.push("Game name", info.game_name.clone());
    t.push("Region", info.region.clone());
    if let Some(date) = &info.apploader_date {
        t.push("Apploader date", date.clone());
    }
    t.push("Physical bytes", format!("{}", info.physical_bytes));
    order_rom(&mut t);

    let mut out = String::new();
    section(&mut out, "ROM", &t);

    if !info.fst_root.is_empty() {
        let mut inner = String::new();
        for e in &info.fst_root {
            if e.is_dir {
                inner.push_str(&format!("  {}/\n", e.name));
            } else {
                inner.push_str(&format!("  {}  {} bytes\n", e.name, e.size));
            }
        }
        let total = info.fst_file_count as usize + info.fst_dir_count as usize;
        if total > info.fst_root.len() {
            inner.push_str(&format!(
                "  {} files, {} dirs\n",
                info.fst_file_count, info.fst_dir_count
            ));
        }
        nested(&mut out, "Inner files", &inner);
    }

    if let Some(banner) = &info.banner {
        out.push_str(&format!("Banner format: {}\n\n", banner.format));
        if !banner.titles.is_empty() {
            out.push_str("Banner titles:\n");
            for t in &banner.titles {
                out.push_str(&format!(
                    "  {:<10}  {} ({})\n    {}\n",
                    t.language,
                    t.long_game_name,
                    t.long_maker,
                    t.description.replace('\n', " ")
                ));
            }
            out.push('\n');
        }
    }

    if let Some(img) = &info.banner_image {
        out.push_str(&format!(
            "Banner image: {}x{} PNG ({} bytes)\n\n",
            img.width,
            img.height,
            img.png_bytes.len()
        ));
    }

    out
}

fn render_rvl(info: &rom_converto_lib::info::RvlInfo) -> String {
    let mut t = KeyValueTable::new();
    t.push("Format", format!("Wii ({})", info.container));
    t.push("Content type", "Game");
    t.push("Game ID", info.game_id.clone());
    t.push(
        "Maker code",
        format_maker(&info.maker_code, info.maker_name.as_deref()),
    );
    t.push("Disc number", format!("{}", info.disc_number));
    t.push("Disc version", format!("{}", info.disc_version));
    t.push("Game name", info.game_name.clone());
    t.push("Region", info.region.clone());
    t.push("Physical bytes", format!("{}", info.physical_bytes));
    if let Some(tmd) = &info.tmd {
        t.push("Title ID", format!("{:016X}", tmd.title_id));
        t.push("Title version", format!("{}", tmd.title_version));
        t.push("System version", format!("{:016X}", tmd.system_version));
        if let Some(ios) = tmd.ios_slot {
            t.push("IOS slot", format!("IOS{}", ios));
        }
        t.push("TMD region", tmd.region_name.clone());
        t.push("Content count", format!("{}", tmd.content_count));
        t.push("Access rights", format!("0x{:08X}", tmd.access_rights));
    }
    order_rom(&mut t);

    let mut out = String::new();
    section(&mut out, "ROM", &t);

    if !info.partitions.is_empty() {
        let mut inner = String::from("Partitions:\n");
        for p in &info.partitions {
            inner.push_str(&format!(
                "  group={} type={} ({:<7})  offset=0x{:X}\n",
                p.group, p.partition_type, p.kind, p.offset
            ));
        }
        nested(&mut out, "Inner files", &inner);
    }

    if let Some(names) = &info.imet_names
        && !names.is_empty()
    {
        out.push_str("IMET banner names:\n");
        for (lang, name) in &names.entries {
            out.push_str(&format!("  {:<10?}  {}\n", lang, name));
        }
        out.push('\n');
    }

    out
}

/// "disc"/"nus" sources ship encrypted; "loadiine"/"wua" are already
/// decrypted extractions.
fn wup_encryption(source_kind: &str) -> Option<&'static str> {
    if source_kind.starts_with("disc") || source_kind.starts_with("nus") {
        Some("encrypted")
    } else if source_kind.starts_with("loadiine") || source_kind.starts_with("wua") {
        Some("decrypted")
    } else {
        None
    }
}

fn render_wup(info: &rom_converto_lib::info::WupInfo) -> String {
    let mut t = KeyValueTable::new();
    t.push("Format", format!("Wii U ({})", info.source_kind));
    t.push("Title ID", info.title_id_hex.clone());
    t.push("Content type", info.title_type.clone());
    if let Some(e) = wup_encryption(&info.source_kind) {
        t.push("Encryption", e);
    }
    if let Some(uv) = info.update_version {
        t.push(
            "Title version",
            format!("v{} (base v{})", uv, info.title_version),
        );
    } else {
        t.push("Title version", format!("v{}", info.title_version));
    }
    if let Some(meta) = &info.meta
        && !meta.region_names.is_empty()
    {
        t.push("Region", meta.region_names.join(", "));
    }
    t.push("Group ID", format!("0x{:04X}", info.group_id));
    t.push("Access rights", format!("0x{:08X}", info.access_rights));
    if info.content_count > 0 {
        t.push("Content count", format!("{}", info.content_count));
        t.push(
            "Total content size",
            format!("{} bytes", info.total_content_size),
        );
    }
    if let Some(os) = info.os_version {
        t.push("OS version", format!("{:016X}", os));
    }
    if let Some(sdk) = info.sdk_version {
        t.push("SDK version", format!("{}", sdk));
    }
    order_rom(&mut t);

    let mut out = String::new();
    section(&mut out, "ROM", &t);

    let mut inner = String::new();
    if !info.bundled_titles.is_empty() {
        inner.push_str("Bundled titles:\n");
        for bt in &info.bundled_titles {
            inner.push_str(&format!(
                "  {}  {:<8}  v{}\n",
                bt.title_id_hex, bt.title_type, bt.title_version
            ));
        }
    }
    if !info.disc_partitions.is_empty() {
        if !inner.is_empty() {
            inner.push('\n');
        }
        inner.push_str("Disc partitions:\n");
        for p in &info.disc_partitions {
            inner.push_str(&format!(
                "  {}  {}  sector {}\n",
                p.name, p.kind, p.start_sector
            ));
        }
    }
    if !inner.is_empty() {
        nested(&mut out, "Inner files", &inner);
    }

    if let Some(meta) = &info.meta {
        if let Some(code) = &meta.product_code {
            out.push_str(&format!("Product code: {}\n", code));
        }
        if let Some(code) = &meta.company_code {
            out.push_str(&format!(
                "Company code: {}\n",
                format_maker(code, meta.company_name.as_deref())
            ));
        }
        if let Some(s) = meta.save_size {
            out.push_str(&format!("Save data size: {} bytes\n", s));
        }
        if let Some(s) = meta.common_save_size {
            out.push_str(&format!("Common save size: {} bytes\n", s));
        }
        if let Some(s) = meta.account_save_size {
            out.push_str(&format!("Account save size: {} bytes\n", s));
        }
        if let Some(b) = meta.network_use {
            out.push_str(&format!("Network use: {}\n", b));
        }
        if let Some(b) = meta.online_account_use {
            out.push_str(&format!("Online account use: {}\n", b));
        }
        if let Some(d) = &meta.mastering_date {
            out.push_str(&format!("Mastering date: {}\n", d));
        }
        if let Some(b) = meta.drc_use {
            out.push_str(&format!("GamePad required (drc_use): {}\n", b));
        }
        if let Some(os) = meta.os_version {
            out.push_str(&format!("OS version (meta): {:016X}\n", os));
        }
        if let Some(sz) = meta.app_size.filter(|s| *s > 0) {
            out.push_str(&format!("App size (meta): {} bytes\n", sz));
        }
        if let Some(g) = meta.group_id {
            out.push_str(&format!("Group ID (meta): 0x{:08X}\n", g));
        }
        let mut accessories: Vec<&'static str> = Vec::new();
        if meta.ext_dev_nunchaku == Some(true) {
            accessories.push("Nunchuk");
        }
        if meta.ext_dev_classic == Some(true) {
            accessories.push("Classic Controller");
        }
        if meta.ext_dev_urcc == Some(true) {
            accessories.push("URCC");
        }
        if meta.ext_dev_board == Some(true) {
            accessories.push("Balance Board");
        }
        if meta.ext_dev_usb_keyboard == Some(true) {
            accessories.push("USB Keyboard");
        }
        if !accessories.is_empty() {
            out.push_str(&format!("Accessories: {}\n", accessories.join(", ")));
        }
        out.push('\n');
        if !meta.long_names.is_empty() {
            out.push_str("Long names:\n");
            for (lang, name) in &meta.long_names.entries {
                out.push_str(&format!("  {:<22?}  {}\n", lang, name));
            }
            out.push('\n');
        }
        if !meta.publishers.is_empty() {
            out.push_str("Publishers:\n");
            for (lang, name) in &meta.publishers.entries {
                out.push_str(&format!("  {:<22?}  {}\n", lang, name));
            }
            out.push('\n');
        }
        if !meta.age_ratings.is_empty() {
            out.push_str("Age ratings:\n");
            let mut keys: Vec<&String> = meta.age_ratings.keys().collect();
            keys.sort();
            for k in keys {
                out.push_str(&format!("  {:<10}  {}\n", k, meta.age_ratings[k]));
            }
            out.push('\n');
        }
    }

    out
}

fn render_nx(info: &rom_converto_lib::info::NxInfo) -> String {
    use rom_converto_lib::nintendo::nx::info::NxContainerKind;

    let kind_str = match info.container_kind {
        NxContainerKind::Nsp => "NSP",
        NxContainerKind::Nsz => "NSZ",
        NxContainerKind::Xci => "XCI",
        NxContainerKind::Xcz => "XCZ",
    };

    let mut c = KeyValueTable::new();
    c.push("Format", format!("Switch {}", kind_str));
    c.push(
        "Compressed",
        if info.is_compressed {
            "yes (zstd)"
        } else {
            "no"
        },
    );
    c.push("Distribution", info.distribution.display_name());
    c.push("Structure", info.structure.display_name());
    c.push("Physical bytes", format!("{}", info.physical_bytes));
    c.push("Files", format!("{}", info.files.len()));
    c.push("NCA files", format!("{}", info.nca_names.len()));
    c.push("CNMT NCAs", format!("{}", info.cnmt_nca_names.len()));
    c.push("Tickets", format!("{}", info.tickets.len()));
    c.push(
        "Encryption",
        match info.container_kind {
            // NCZ crypto sections are stored decrypted; the AES-CTR is
            // re-applied on read.
            NxContainerKind::Nsz | NxContainerKind::Xcz => "decrypted (ncz sections)",
            NxContainerKind::Nsp | NxContainerKind::Xci => {
                if info.tickets.is_empty() {
                    "encrypted (standard keys)"
                } else {
                    "encrypted (titlekey)"
                }
            }
        },
    );

    let mut t = KeyValueTable::new();
    if let Some(full) = &info.full {
        t.push("Title ID", format!("{:016X}", full.application_title_id));
        t.push("Content type", full.title_kind.display_name());
        t.push(
            "Title version",
            format!("{} (0x{:x})", full.title_version, full.title_version),
        );
        t.push(
            "Required system version",
            format!(
                "{} (0x{:x})",
                full.required_system_version, full.required_system_version
            ),
        );
        if let Some(rav) = full.required_application_version {
            t.push("Required application version", format!("{}", rav));
        }
        if let Some(base) = full.base_application_id {
            t.push("Base game", format!("{:016X}", base));
        }
        t.push("Storage ID", format!("{}", full.storage_id));
        t.push("Attributes", format!("0x{:02x}", full.attributes));
        t.push("Content count", format!("{}", full.content_count));
        t.push(
            "Total content size",
            format!("{} bytes", full.total_content_size),
        );
    } else {
        t.push(
            "Decryption",
            "limited (prod.keys not loaded or not provided)".to_string(),
        );
    }
    order_rom(&mut t);

    let mut out = String::new();
    section(&mut out, "Container", &c);
    section(&mut out, "ROM", &t);

    let mut inner = String::new();
    if let Some(parts) = &info.xci_partitions {
        inner.push_str("XCI partitions:\n");
        for p in parts {
            inner.push_str(&format!(
                "  {:<8} {} files, {} bytes\n",
                p.name, p.file_count, p.total_size
            ));
        }
    }
    if let Some(full) = &info.full
        && !full.contents.is_empty()
    {
        if !inner.is_empty() {
            inner.push('\n');
        }
        inner.push_str("CNMT contents:\n");
        for c in &full.contents {
            inner.push_str(&format!(
                "  {:<10}  {:>12} bytes  id={}\n",
                c.content_type, c.size, c.content_id
            ));
        }
    }
    if !info.cnmt_nca_names.is_empty() {
        if !inner.is_empty() {
            inner.push('\n');
        }
        inner.push_str("CNMT NCAs:\n");
        for n in &info.cnmt_nca_names {
            inner.push_str(&format!("  {}\n", n));
        }
    }
    if !inner.is_empty() {
        nested(&mut out, "Inner files", &inner);
    }

    if !info.tickets.is_empty() {
        out.push_str("Tickets:\n");
        for tk in &info.tickets {
            out.push_str(&format!(
                "  {:<40}  rights_id={}  master_key_rev={}\n",
                tk.file_name, tk.rights_id, tk.master_key_revision
            ));
        }
        out.push('\n');
    }

    if let Some(full) = &info.full {
        if !full.related_titles.is_empty() {
            out.push_str("Related titles:\n");
            for r in &full.related_titles {
                out.push_str(&format!(
                    "  {:016X}  {:<14}  v{}\n",
                    r.title_id,
                    r.kind.display_name(),
                    r.version
                ));
            }
            out.push('\n');
        }
        if let Some(ctrl) = &full.control {
            out.push_str(&format!("Display version: {}\n", ctrl.display_version));
            out.push_str(&format!(
                "Startup user account: {}\n",
                ctrl.startup_user_account_name
            ));
            out.push_str(&format!("Video capture: {}\n", ctrl.video_capture_name));
            out.push_str(&format!(
                "Screen orientation: {}\n",
                ctrl.screen_orientation_name
            ));
            out.push_str(&format!(
                "Add-on install policy: {}\n",
                ctrl.addon_install_policy_name
            ));
            if !ctrl.attributes.is_empty() {
                out.push_str(&format!("Attributes: {}\n", ctrl.attributes.join(", ")));
            }
            if !ctrl.parental_control_flags.is_empty() {
                out.push_str(&format!(
                    "Parental control: {}\n",
                    ctrl.parental_control_flags.join(", ")
                ));
            }
            if !ctrl.supported_languages.is_empty() {
                out.push_str(&format!(
                    "Languages: {}\n",
                    ctrl.supported_languages.join(", ")
                ));
            }
            out.push('\n');
            if !ctrl.age_ratings.is_empty() {
                out.push_str("Age ratings:\n");
                for r in &ctrl.age_ratings {
                    out.push_str(&format!("  {:<14}  {}\n", r.organization, r.age));
                }
                out.push('\n');
            }
            if !ctrl.titles.is_empty() {
                out.push_str("Titles:\n");
                for t in &ctrl.titles {
                    out.push_str(&format!(
                        "  {:<22}  {}  ({})\n",
                        t.language, t.name, t.publisher
                    ));
                }
                out.push('\n');
            }
            out.push_str("Save data sizes (bytes):\n");
            out.push_str(&format!("  user             {}\n", ctrl.user_account_save));
            out.push_str(&format!(
                "  user journal     {}\n",
                ctrl.user_account_save_journal
            ));
            out.push_str(&format!("  device           {}\n", ctrl.device_save));
            out.push_str(&format!(
                "  device journal   {}\n",
                ctrl.device_save_journal
            ));
            out.push_str(&format!("  bcat             {}\n", ctrl.bcat_save));
            out.push('\n');
            if let Some(lang) = &ctrl.icon_language
                && let Some(img) = &ctrl.icon
            {
                out.push_str(&format!(
                    "Icon: {}x{} PNG ({} bytes, language {})\n\n",
                    img.width,
                    img.height,
                    img.png_bytes.len(),
                    lang
                ));
            }
        }
    }

    out
}

fn render_xbox(info: &rom_converto_lib::info::XisoInfo) -> String {
    use rom_converto_lib::microsoft::xdvdfs::PartitionKind;
    let kind = match info.kind {
        PartitionKind::Trimmed => "trimmed".to_string(),
        PartitionKind::Xgd1 => "XGD1".to_string(),
        PartitionKind::Xgd2 => "XGD2".to_string(),
        PartitionKind::Xgd3 => "XGD3".to_string(),
        PartitionKind::X360Extra(base) => format!("X360 extra (base 0x{:X})", base),
    };

    let mut c = KeyValueTable::new();
    c.push("Format", "Xbox XISO");
    c.push("Partition kind", kind);
    c.push("Base offset", format!("0x{:X}", info.base));
    c.push("Root sector", format!("{}", info.root_sector));
    c.push("Root size", format!("{} bytes", info.root_size));
    c.push("Files", format!("{}", info.file_count));
    c.push("Directories", format!("{}", info.dir_count));
    c.push("Total file bytes", format!("{}", info.total_file_bytes));
    c.push("Image size", format!("{} bytes", info.image_size));

    let mut t = KeyValueTable::new();
    if info.xbe.is_some() || info.xex.is_some() {
        t.push("Content type", "Game");
    }
    if let Some(xbe) = &info.xbe {
        t.push("Title name", xbe.title_name.clone());
        t.push(
            "Title ID",
            format!("{} ({})", xbe.title_id_hex, xbe.title_id_code),
        );
        t.push("Version", format!("{}", xbe.version));
        t.push("Disc number", format!("{}", xbe.disc_number));
        t.push(
            "Region",
            if xbe.region_names.is_empty() {
                format!("0x{:08X}", xbe.region)
            } else {
                xbe.region_names.join(", ")
            },
        );
        t.push("Allowed media", xbe.allowed_media_names.join(", "));
        if let Some(img) = &xbe.icon {
            t.push(
                "Icon",
                format!(
                    "{}x{} PNG ({} bytes)",
                    img.width,
                    img.height,
                    img.png_bytes.len()
                ),
            );
        }
        t.push("Ratings", format!("0x{:08X}", xbe.ratings));
        t.push("Cert timestamp", format!("{}", xbe.cert_timestamp));
        if !xbe.alternate_title_ids.is_empty() {
            t.push(
                "Alternate title IDs",
                xbe.alternate_title_ids
                    .iter()
                    .map(|id| format!("{:08X}", id))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
    }
    if let Some(xex) = &info.xex {
        push_xex_rows(&mut t, xex, info.xbe.is_none());
    }
    order_rom(&mut t);

    let mut out = String::new();
    section(&mut out, "Container", &c);
    section(&mut out, "ROM", &t);

    if !info.root_entries.is_empty() {
        let mut inner = String::new();
        for e in &info.root_entries {
            if e.is_dir {
                inner.push_str(&format!("  {}/\n", e.name));
            } else {
                inner.push_str(&format!("  {}  {} bytes\n", e.name, e.size));
            }
        }
        nested(&mut out, "Inner files", &inner);
    }

    out
}

fn render_ps3(info: &rom_converto_lib::info::Ps3Info) -> String {
    let mut t = KeyValueTable::new();
    t.push("Format", "PS3 ISO");
    t.push("Content type", "Game");
    if let Some(v) = &info.title {
        t.push("Title", v.clone());
    }
    if let Some(v) = &info.title_id {
        t.push("Title ID", v.clone());
    }
    if let Some(v) = &info.region {
        t.push("Region", v.clone());
    }
    if let Some(v) = &info.version {
        t.push("Version", v.clone());
    }
    if let Some(v) = &info.app_ver {
        t.push("App version", v.clone());
    }
    t.push("Physical bytes", format!("{}", info.size_bytes));
    t.push("Regions", format!("{}", info.region_count));
    t.push("Total sectors", format!("{}", info.total_sectors));
    if let Some(encrypted) = info.encrypted {
        t.push(
            "Encryption",
            if encrypted { "encrypted" } else { "decrypted" },
        );
    }
    t.push("Encrypted sectors", format!("{}", info.encrypted_sectors));
    if let Some(v) = &info.resolution {
        t.push("Resolution", v.clone());
    }
    if let Some(v) = &info.sound_format {
        t.push("Sound format", v.clone());
    }
    if let Some(v) = &info.firmware {
        t.push("Firmware", v.clone());
    }
    if let Some(p) = info.parental_level {
        t.push("Parental level", format!("{}", p));
    }
    if let Some(img) = &info.icon {
        t.push(
            "Icon",
            format!(
                "{}x{} PNG ({} bytes)",
                img.width,
                img.height,
                img.png_bytes.len()
            ),
        );
    }
    order_rom(&mut t);

    let mut out = String::new();
    section(&mut out, "ROM", &t);

    if !info.root_files.is_empty() {
        let mut inner = String::new();
        for e in &info.root_files {
            if e.is_dir {
                inner.push_str(&format!("  {}/\n", e.name));
            } else {
                inner.push_str(&format!("  {}  {} bytes\n", e.name, e.size));
            }
        }
        nested(&mut out, "Inner files", &inner);
    }

    out
}

fn psx_rom_table(info: &rom_converto_lib::info::PsxInfo) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Format", info.disc_kind.clone());
    t.push("Content type", "Game");
    if let Some(v) = &info.title_id {
        t.push("Title ID", v.clone());
    }
    if let Some(v) = &info.boot_executable {
        t.push("Boot executable", v.clone());
    }
    if let Some(v) = &info.volume_id {
        t.push("Volume ID", v.clone());
    }
    if let Some(v) = &info.version {
        t.push("Version", v.clone());
    }
    t.push("Sectors", format!("{}", info.total_sectors));
    t.push("Size", format!("{}", info.size_bytes));
    order_rom(&mut t);
    t
}

fn render_psx(info: &rom_converto_lib::info::PsxInfo) -> String {
    let mut out = String::new();
    section(&mut out, "ROM", &psx_rom_table(info));
    out
}

fn psp_rom_table(info: &rom_converto_lib::info::PspInfo) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Format", "PSP UMD");
    t.push(
        "Content type",
        info.category.clone().unwrap_or_else(|| "Game".to_string()),
    );
    if let Some(v) = &info.title {
        t.push("Title", v.clone());
    }
    if let Some(v) = &info.title_id {
        t.push("Title ID", v.clone());
    }
    if let Some(v) = &info.version {
        t.push("Version", v.clone());
    }
    if let Some(v) = &info.firmware {
        t.push("Firmware", v.clone());
    }
    t.push("Sectors", format!("{}", info.total_sectors));
    t.push("Size", format!("{}", info.size_bytes));
    if let Some(img) = &info.icon {
        t.push(
            "Icon",
            format!(
                "{}x{} PNG ({} bytes)",
                img.width,
                img.height,
                img.png_bytes.len()
            ),
        );
    }
    if let Some(img) = &info.background {
        t.push(
            "Background",
            format!(
                "{}x{} PNG ({} bytes)",
                img.width,
                img.height,
                img.png_bytes.len()
            ),
        );
    }
    order_rom(&mut t);
    t
}

fn render_psp(info: &rom_converto_lib::info::PspInfo) -> String {
    let mut out = String::new();
    section(&mut out, "ROM", &psp_rom_table(info));
    out
}

fn render_laserdisc(info: &rom_converto_lib::info::LdAviInfo) -> String {
    use rom_converto_lib::laserdisc::info::LdDiscType;

    let mut t = KeyValueTable::new();
    t.push("Format", format!("LaserDisc AVI ({})", info.video_fourcc));
    t.push(
        "Video",
        format!(
            "{}x{} @ {:.3} fps, {} frames",
            info.video_width, info.video_height, info.fps, info.frame_count
        ),
    );
    t.push("Duration", format!("{:.1}s", info.duration_seconds));
    t.push(
        "Audio",
        format!(
            "{} ch, {} Hz, {}-bit, {} samples",
            info.audio_channels, info.audio_rate, info.audio_bits, info.audio_sample_count
        ),
    );
    t.push("Size", format!("{}", info.file_size_bytes));
    let mut out = t.render();

    out.push_str("\nCHD projection:\n");
    let mut p = KeyValueTable::new();
    p.push("Interlaced", if info.interlaced { "yes" } else { "no" });
    p.push(
        "Field size",
        format!("{}x{}", info.video_width, info.field_height),
    );
    p.push("Fields", format!("{}", info.fields));
    p.push(
        "Max samples/field",
        format!("{}", info.max_samples_per_field),
    );
    p.push("Hunk bytes", format!("{}", info.bytes_per_frame));
    p.push("AVAV metadata", info.av_metadata.clone());
    out.push_str(&p.render());

    if let Some(vbi) = &info.vbi {
        out.push_str("\nVBI:\n");
        let mut v = KeyValueTable::new();
        v.push("Fields scanned", format!("{}", vbi.fields_scanned));
        v.push("White flags", format!("{}", vbi.white_flag_count));
        v.push(
            "Lead-in / lead-out",
            format!("{} / {}", vbi.lead_in, vbi.lead_out),
        );
        v.push(
            "Disc type",
            match vbi.disc_type {
                LdDiscType::Cav => "CAV",
                LdDiscType::Clv => "CLV",
                LdDiscType::Unknown => "unknown",
            },
        );
        if let (Some(min), Some(max)) = (vbi.cav_picture_min, vbi.cav_picture_max) {
            v.push("CAV picture range", format!("{}-{}", min, max));
        }
        if let (Some(start), Some(end)) = (&vbi.clv_start, &vbi.clv_end) {
            v.push(
                "CLV timecode range",
                format!(
                    "{:02}:{:02}-{:02}:{:02}",
                    start.hours, start.minutes, end.hours, end.minutes
                ),
            );
        }
        if let (Some(min), Some(max)) = (vbi.chapter_min, vbi.chapter_max) {
            v.push("Chapters", format!("{}-{}", min, max));
        }
        v.push(
            "Fields without code",
            format!("{}", vbi.fields_without_code),
        );
        out.push_str(&v.render());
    }

    out
}

fn render_xenon(info: &rom_converto_lib::info::ZarInfo) -> String {
    let mut c = KeyValueTable::new();
    c.push("Format", "Xbox 360 ZArchive");
    c.push("Files", format!("{}", info.file_count));
    c.push("Directories", format!("{}", info.dir_count));
    c.push("Logical bytes", format!("{}", info.logical_size));
    c.push("Compressed bytes", format!("{}", info.compressed_size));
    c.push("Blocks", format!("{}", info.block_count));
    c.push(
        "default.xex",
        if info.has_default_xex {
            "present"
        } else {
            "not found"
        },
    );

    let mut t = KeyValueTable::new();
    if let Some(xex) = &info.xex {
        t.push("Content type", "Game");
        push_xex_rows(&mut t, xex, true);
    }
    order_rom(&mut t);

    let mut out = String::new();
    section(&mut out, "Container", &c);
    section(&mut out, "ROM", &t);

    if !info.root_entries.is_empty() {
        let mut inner = String::new();
        for e in &info.root_entries {
            if e.is_file {
                inner.push_str(&format!("  {}  {} bytes\n", e.name, e.size));
            } else {
                inner.push_str(&format!("  {}/\n", e.name));
            }
        }
        nested(&mut out, "Inner files", &inner);
    }

    out
}

fn push_xex_rows(t: &mut KeyValueTable, xex: &XexInfo, include_shared: bool) {
    if let Some(name) = &xex.title_name {
        t.push("Title name", name.clone());
    }
    if include_shared {
        t.push("Title ID", xex.title_id_hex.clone());
    }
    t.push("Media ID", format!("{:08X}", xex.media_id));
    if include_shared {
        t.push("Version", xex.version.clone());
    }
    t.push("Base version", xex.base_version.clone());
    t.push("Disc", format!("{}/{}", xex.disc_number, xex.disc_count));
    t.push("Platform", format!("{}", xex.platform));
    if let Some(pe) = &xex.original_pe_name {
        t.push("Original PE name", pe.clone());
    }
    if include_shared {
        t.push("Region", xex.region_names.join(", "));
    }
    t.push("Allowed media", format!("0x{:08X}", xex.allowed_media));
    if include_shared && let Some(img) = &xex.icon {
        t.push(
            "Icon",
            format!(
                "{}x{} PNG ({} bytes)",
                img.width,
                img.height,
                img.png_bytes.len()
            ),
        );
    }
}

fn render_nds(info: &rom_converto_lib::info::NdsInfo) -> String {
    use rom_converto_lib::nintendo::nds::info::NdsSecureAreaState;

    let mut c = KeyValueTable::new();
    c.push("Physical bytes", format!("{}", info.physical_bytes));
    c.push(
        "Cartridge capacity",
        format!(
            "{} bytes (code {})",
            info.capacity_bytes, info.device_capacity
        ),
    );
    c.push("NTR ROM size", format!("{}", info.ntr_rom_size));
    c.push(
        "ARM9",
        format!(
            "rom=0x{:X} entry=0x{:08X} load=0x{:08X} size={}",
            info.arm9.rom_offset, info.arm9.entry_address, info.arm9.load_address, info.arm9.size
        ),
    );
    c.push(
        "ARM7",
        format!(
            "rom=0x{:X} entry=0x{:08X} load=0x{:08X} size={}",
            info.arm7.rom_offset, info.arm7.entry_address, info.arm7.load_address, info.arm7.size
        ),
    );
    c.push(
        "File name table",
        format!("offset=0x{:X} size={}", info.fnt_offset, info.fnt_size),
    );
    c.push(
        "File allocation table",
        format!("offset=0x{:X} size={}", info.fat_offset, info.fat_size),
    );
    c.push(
        "Header CRC16",
        format!(
            "0x{:04X} (computed 0x{:04X}, {})",
            info.header_crc16,
            info.header_crc16_computed,
            if info.header_crc16_valid {
                "valid"
            } else {
                "invalid"
            }
        ),
    );
    c.push(
        "Secure area",
        match info.secure_area {
            NdsSecureAreaState::NotPresent => "not present",
            NdsSecureAreaState::Encrypted => "encrypted",
            NdsSecureAreaState::Decrypted => "decrypted",
        },
    );

    let mut t = KeyValueTable::new();
    t.push("Format", format!("Nintendo DS ({})", info.unit_code_name));
    t.push("Content type", "Game");
    if let Some(name) = info.banner.as_ref().and_then(|b| b.titles.primary()) {
        t.push("Title", name.replace('\n', " "));
    }
    t.push("Game title", info.game_title.clone());
    t.push("Title ID", info.game_code.clone());
    t.push("Maker code", info.maker_code.clone());
    t.push("Region", format!("0x{:02X}", info.region));
    t.push("Version", format!("{}", info.rom_version));
    t.push("Size", format!("{}", info.physical_bytes));
    if let Some(banner) = &info.banner {
        t.push(
            "Icon",
            format!(
                "{}x{} PNG ({} bytes)",
                banner.icon.width,
                banner.icon.height,
                banner.icon.png_bytes.len()
            ),
        );
    }
    order_rom(&mut t);

    let mut out = String::new();
    section(&mut out, "Container", &c);
    section(&mut out, "ROM", &t);

    if let Some(banner) = &info.banner {
        out.push_str(&format!(
            "Banner version: {} (CRC16 0x{:04X}, computed 0x{:04X}, {})\n\n",
            banner.banner_version,
            banner.banner_crc16,
            banner.banner_crc16_computed,
            if banner.banner_crc16_valid {
                "valid"
            } else {
                "invalid"
            }
        ));
        if !banner.titles.is_empty() {
            out.push_str("Banner titles:\n");
            for (lang, name) in &banner.titles.entries {
                out.push_str(&format!("  {:<10?}  {}\n", lang, name.replace('\n', " ")));
            }
            out.push('\n');
        }
    }

    out
}

fn push_checksum(t: &mut KeyValueTable, stored: String, computed: String, valid: bool) {
    t.push("Checksum", stored);
    t.push("Computed checksum", computed);
    t.push("Checksum valid", if valid { "yes" } else { "no" });
}

fn yes_no(v: bool) -> &'static str {
    if v { "yes" } else { "no" }
}

fn render_retro(info: &rom_converto_lib::info::RetroInfo) -> String {
    use rom_converto_lib::retro::RetroDetails;

    let (system, mut t) = match &info.details {
        RetroDetails::Nes(n) => ("NES", retro_nes(n)),
        RetroDetails::Snes(s) => ("SNES", retro_snes(s)),
        RetroDetails::N64(n) => ("Nintendo 64", retro_n64(n)),
        RetroDetails::GameBoy(g) => ("Game Boy", retro_gb(g)),
        RetroDetails::Gba(g) => ("Game Boy Advance", retro_gba(g)),
        RetroDetails::MegaDrive(m) => ("Mega Drive", retro_md(m)),
        RetroDetails::MasterSystem(s) => ("Master System", retro_sms(s)),
        RetroDetails::GameGear(s) => ("Game Gear", retro_sms(s)),
        RetroDetails::VirtualBoy(v) => ("Virtual Boy", retro_vb(v)),
        RetroDetails::WonderSwan(w) => ("WonderSwan", retro_ws(w)),
        RetroDetails::NeoGeoPocket(n) => ("Neo Geo Pocket", retro_ngp(n)),
        RetroDetails::Lynx(l) => ("Atari Lynx", retro_lynx(l)),
        RetroDetails::Atari7800(a) => ("Atari 7800", retro_a78(a)),
    };
    t.push("Format", system);
    t.push("Content type", "Game");
    t.push("Size", format!("{}", info.file_size));
    order_rom(&mut t);

    let mut out = String::new();
    section(&mut out, "ROM", &t);
    out
}

fn retro_nes(info: &rom_converto_lib::retro::NesInfo) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Header", if info.nes2 { "NES 2.0" } else { "iNES" });
    t.push(
        "Mapper",
        match info.submapper {
            Some(sub) => format!("{} (submapper {})", info.mapper, sub),
            None => format!("{}", info.mapper),
        },
    );
    t.push("PRG ROM", format!("{} bytes", info.prg_rom_bytes));
    t.push("CHR ROM", format!("{} bytes", info.chr_rom_bytes));
    t.push("Mirroring", info.mirroring.clone());
    t.push("Battery", yes_no(info.battery));
    t.push("Trainer", yes_no(info.trainer));
    t.push("Four screen", yes_no(info.four_screen));
    t.push("Console type", info.console_type.clone());
    t.push("Timing", info.timing.clone());
    for (key, value) in [
        ("PRG RAM", info.prg_ram_bytes),
        ("PRG NVRAM", info.prg_nvram_bytes),
        ("CHR RAM", info.chr_ram_bytes),
        ("CHR NVRAM", info.chr_nvram_bytes),
    ] {
        if let Some(bytes) = value {
            t.push(key, format!("{} bytes", bytes));
        }
    }
    t
}

fn retro_snes(info: &rom_converto_lib::retro::SnesInfo) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Title", info.title.clone());
    t.push("Mapping", info.mapping.clone());
    t.push("Copier header", yes_no(info.copier_header));
    t.push("Header offset", format!("0x{:X}", info.header_offset));
    t.push("Map mode", format!("0x{:02X}", info.map_mode));
    t.push("FastROM", yes_no(info.fastrom));
    t.push("Chipset", format!("0x{:02X}", info.chipset));
    if let Some(co) = &info.coprocessor {
        t.push("Coprocessor", co.clone());
    }
    t.push("ROM size", format!("{} KiB", info.rom_size_kb));
    t.push("SRAM size", format!("{} KiB", info.sram_size_kb));
    t.push(
        "Region",
        match &info.region {
            Some(r) => format!("{} (0x{:02X})", r, info.country),
            None => format!("0x{:02X}", info.country),
        },
    );
    t.push("Licensee", format!("0x{:02X}", info.licensee));
    t.push("Version", format!("{}", info.version));
    push_checksum(
        &mut t,
        format!(
            "0x{:04X} (complement 0x{:04X})",
            info.checksum, info.checksum_complement
        ),
        format!("0x{:04X}", info.computed_checksum),
        info.checksum_valid,
    );
    t
}

fn retro_n64(info: &rom_converto_lib::retro::N64Info) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Title", info.internal_name.clone());
    t.push("Title ID", info.game_id.clone());
    t.push("Byte order", info.byte_order.clone());
    t.push("Media", info.media.clone());
    t.push(
        "Region",
        match &info.region {
            Some(r) => format!("{} ({})", r, info.region_code),
            None => info.region_code.clone(),
        },
    );
    t.push("Version", format!("{}", info.version));
    t.push("CRC1", info.crc1.clone());
    t.push("CRC2", info.crc2.clone());
    t.push("Boot code CRC32", info.bootcode_crc32.clone());
    if let Some(cic) = &info.cic {
        t.push("CIC", cic.clone());
    }
    t
}

fn retro_gb(info: &rom_converto_lib::retro::GbInfo) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Title", info.title.clone());
    if let Some(code) = &info.manufacturer_code {
        t.push("Manufacturer code", code.clone());
    }
    t.push(
        "CGB",
        match &info.cgb {
            Some(c) => format!("{} (0x{:02X})", c, info.cgb_flag),
            None => format!("0x{:02X}", info.cgb_flag),
        },
    );
    t.push("SGB flag", format!("0x{:02X}", info.sgb_flag));
    t.push(
        "Cartridge type",
        match &info.cart_type_name {
            Some(n) => format!("{} (0x{:02X})", n, info.cart_type),
            None => format!("0x{:02X}", info.cart_type),
        },
    );
    if let Some(bytes) = info.rom_bytes {
        t.push("ROM size", format!("{} bytes", bytes));
    }
    if let Some(bytes) = info.ram_bytes {
        t.push("RAM size", format!("{} bytes", bytes));
    }
    t.push(
        "Destination",
        match &info.destination_name {
            Some(n) => format!("{} (0x{:02X})", n, info.destination),
            None => format!("0x{:02X}", info.destination),
        },
    );
    t.push("Licensee", info.licensee.clone());
    t.push("Version", format!("{}", info.version));
    t.push("Logo valid", yes_no(info.logo_valid));
    t.push(
        "Header checksum",
        format!(
            "0x{:02X} (computed 0x{:02X}, {})",
            info.header_checksum,
            info.computed_header_checksum,
            if info.header_checksum_valid {
                "valid"
            } else {
                "invalid"
            }
        ),
    );
    push_checksum(
        &mut t,
        format!("0x{:04X}", info.global_checksum),
        format!("0x{:04X}", info.computed_global_checksum),
        info.global_checksum_valid,
    );
    t
}

fn retro_gba(info: &rom_converto_lib::retro::GbaInfo) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Title", info.title.clone());
    t.push("Title ID", info.game_code.clone());
    if let Some(region) = &info.region {
        t.push("Region", region.clone());
    }
    t.push("Maker code", info.maker_code.clone());
    t.push("Version", format!("{}", info.version));
    t.push("Logo valid", yes_no(info.logo_valid));
    push_checksum(
        &mut t,
        format!("0x{:02X}", info.header_checksum),
        format!("0x{:02X}", info.computed_header_checksum),
        info.header_checksum_valid,
    );
    t
}

fn retro_md(info: &rom_converto_lib::retro::MdInfo) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Title", info.overseas_title.clone());
    t.push("Domestic title", info.domestic_title.clone());
    t.push("Title ID", info.serial.clone());
    t.push("Layout", info.format.clone());
    t.push("Console", info.console.clone());
    t.push("Copyright", info.copyright.clone());
    t.push("Device support", info.device_support.join(", "));
    t.push("Region", info.region.join(", "));
    t.push(
        "ROM range",
        format!("0x{:X}..0x{:X}", info.rom_start, info.rom_end),
    );
    push_checksum(
        &mut t,
        format!("0x{:04X}", info.checksum),
        format!("0x{:04X}", info.computed_checksum),
        info.checksum_valid,
    );
    t
}

fn retro_sms(info: &rom_converto_lib::retro::SmsInfo) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Title ID", format!("{}", info.product_code));
    t.push("Header offset", format!("0x{:X}", info.header_offset));
    t.push("Version", format!("{}", info.version));
    t.push(
        "Region",
        match &info.region {
            Some(r) => format!("{} (0x{:X})", r, info.region_code),
            None => format!("0x{:X}", info.region_code),
        },
    );
    t.push(
        "ROM size",
        match info.rom_size_kb {
            Some(kb) => format!("{} KiB (code 0x{:X})", kb, info.rom_size_code),
            None => format!("code 0x{:X}", info.rom_size_code),
        },
    );
    push_checksum(
        &mut t,
        format!("0x{:04X}", info.checksum),
        format!("0x{:04X}", info.computed_checksum),
        info.checksum_valid,
    );
    t
}

fn retro_vb(info: &rom_converto_lib::retro::VbInfo) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Title", info.title.clone());
    t.push("Title ID", info.game_code.clone());
    t.push("Maker code", info.maker_code.clone());
    t.push("Version", format!("{}", info.version));
    t
}

fn retro_ws(info: &rom_converto_lib::retro::WsInfo) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Title ID", format!("0x{:02X}", info.game_id));
    t.push("Publisher ID", format!("0x{:02X}", info.publisher_id));
    t.push("Color", yes_no(info.color));
    t.push(
        "Save",
        match &info.save {
            Some(s) => format!("{} (0x{:02X})", s, info.save_type),
            None => format!("0x{:02X}", info.save_type),
        },
    );
    t.push("Version", format!("{}", info.version));
    push_checksum(
        &mut t,
        format!("0x{:04X}", info.checksum),
        format!("0x{:04X}", info.computed_checksum),
        info.checksum_valid,
    );
    t
}

fn retro_ngp(info: &rom_converto_lib::retro::NgpInfo) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Title", info.title.clone());
    t.push("License", info.license.clone());
    t.push("Startup address", format!("0x{:08X}", info.startup_address));
    t.push("Catalog ID", format!("{}", info.catalog_id));
    t.push("Subcatalog ID", format!("{}", info.subcatalog_id));
    t.push(
        "Machine",
        match &info.machine_name {
            Some(n) => format!("{} (0x{:02X})", n, info.machine),
            None => format!("0x{:02X}", info.machine),
        },
    );
    t
}

fn retro_lynx(info: &rom_converto_lib::retro::LynxInfo) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Title", info.cart_name.clone());
    t.push("Manufacturer", info.manufacturer.clone());
    t.push("Version", format!("{}", info.version));
    t.push("Bank 0 page size", format!("{}", info.bank0_page_size));
    t.push("Bank 1 page size", format!("{}", info.bank1_page_size));
    t.push(
        "Rotation",
        match &info.rotation_name {
            Some(n) => format!("{} ({})", n, info.rotation),
            None => format!("{}", info.rotation),
        },
    );
    t
}

fn retro_a78(info: &rom_converto_lib::retro::A78Info) -> KeyValueTable {
    let mut t = KeyValueTable::new();
    t.push("Title", info.title.clone());
    t.push("Version", format!("{}", info.version));
    t.push("Cart size", format!("{} bytes", info.cart_size));
    t.push("Cart type", format!("0x{:04X}", info.cart_type));
    t.push("Cart features", info.cart_features.join(", "));
    t.push(
        "Controller 1",
        match &info.controller1_name {
            Some(n) => format!("{} ({})", n, info.controller1),
            None => format!("{}", info.controller1),
        },
    );
    t.push(
        "Controller 2",
        match &info.controller2_name {
            Some(n) => format!("{} ({})", n, info.controller2),
            None => format!("{}", info.controller2),
        },
    );
    t.push("TV type", info.tv_type.clone());
    t.push("Save device", format!("{}", info.save_device));
    t
}

/// Spells out what the `DATA.PSAR` magic means, including that an
/// `NPUMDIMG` payload is an encrypted PSN UMD image.
fn psar_kind_label(kind: &rom_converto_lib::sony::psp::PsarKind) -> String {
    use rom_converto_lib::sony::psp::PsarKind;
    match kind {
        PsarKind::Npumdimg => "NPUMDIMG (encrypted PSN UMD image)".to_string(),
        PsarKind::Psisoimg => "PSISOIMG (PS1 Classic disc image)".to_string(),
        PsarKind::Pstitleimg => "PSTITLEIMG (PS1 Classic multi-disc container)".to_string(),
        PsarKind::Unknown { magic } => format!("unknown (magic {})", magic),
    }
}

fn render_pbp(info: &rom_converto_lib::info::PbpInfo) -> String {
    let mut c = KeyValueTable::new();
    c.push("Format", format!("PSP PBP v0x{:X}", info.version));
    c.push("Physical bytes", format!("{}", info.physical_bytes));
    c.push(
        "DATA.PSAR",
        match &info.psar_kind {
            Some(kind) => psar_kind_label(kind),
            None => "absent".to_string(),
        },
    );

    let mut t = KeyValueTable::new();
    if let Some(v) = &info.title {
        t.push("Title", v.clone());
    }
    if let Some(v) = &info.disc_id {
        t.push("Title ID", v.clone());
    }
    if let Some(v) = &info.disc_version {
        t.push("Version", v.clone());
    }
    if let Some(v) = &info.category {
        t.push(
            "Content type",
            match &info.category_label {
                Some(label) => format!("{} ({})", label, v),
                None => v.clone(),
            },
        );
    }
    if let Some(v) = &info.psp_system_ver {
        t.push("Firmware", v.clone());
    }
    if let Some(v) = info.region {
        t.push("Region", format!("0x{:X}", v));
    }
    if let Some(v) = info.parental_level {
        t.push("Parental level", format!("{}", v));
    }
    if let Some(img) = &info.icon {
        t.push(
            "Icon",
            format!(
                "{}x{} PNG ({} bytes)",
                img.width,
                img.height,
                img.png_bytes.len()
            ),
        );
    }
    order_rom(&mut t);

    let mut out = String::new();
    section(&mut out, "Container", &c);
    section(&mut out, "ROM", &t);

    if !info.segments.is_empty() {
        let mut inner = String::new();
        for s in &info.segments {
            if s.present {
                inner.push_str(&format!(
                    "  {:<10}  offset=0x{:X}  {} bytes\n",
                    s.name, s.offset, s.size
                ));
            } else {
                inner.push_str(&format!("  {:<10}  absent\n", s.name));
            }
        }
        nested(&mut out, "Inner files", &inner);
    }

    out
}

fn render_vpk(info: &rom_converto_lib::info::VpkInfo) -> String {
    let mut c = KeyValueTable::new();
    c.push("Format", "PS Vita VPK");
    c.push("Files", format!("{}", info.file_count));
    c.push("Total size", format!("{} bytes", info.total_size));

    let mut t = KeyValueTable::new();
    if let Some(v) = &info.title {
        t.push("Title", v.clone());
    }
    if let Some(v) = &info.title_id {
        t.push("Title ID", v.clone());
    }
    if let Some(v) = &info.content_id {
        t.push("Content ID", v.clone());
    }
    if let Some(v) = &info.app_ver {
        t.push("Version", v.clone());
    }
    if let Some(v) = &info.category {
        t.push(
            "Content type",
            match &info.category_label {
                Some(label) => format!("{} ({})", label, v),
                None => v.clone(),
            },
        );
    }
    if let Some(img) = &info.icon {
        t.push(
            "Icon",
            format!(
                "{}x{} PNG ({} bytes)",
                img.width,
                img.height,
                img.png_bytes.len()
            ),
        );
    }
    order_rom(&mut t);

    let mut out = String::new();
    section(&mut out, "Container", &c);
    section(&mut out, "ROM", &t);
    out
}

fn render_pkg(info: &rom_converto_lib::info::PkgInfo) -> String {
    let mut c = KeyValueTable::new();
    c.push("Format", "PS Vita PKG");
    c.push("Revision", format!("0x{:04X}", info.pkg_revision));
    c.push("Package type", format!("{}", info.pkg_type));
    c.push("Key type", format!("{}", info.key_type));
    c.push("Items", format!("{}", info.item_count));
    c.push("Total size", format!("{} bytes", info.total_size));
    c.push(
        "Data region",
        format!("offset=0x{:X} size={}", info.data_offset, info.data_size),
    );
    if let Some(v) = info.drm_type {
        c.push("DRM type", format!("{}", v));
    }
    if let Some(v) = info.package_flags {
        c.push("Package flags", format!("0x{:08X}", v));
    }
    if !info.meta_ids.is_empty() {
        c.push(
            "Metadata entries",
            info.meta_ids
                .iter()
                .map(|id| format!("0x{:X}", id))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }

    let mut t = KeyValueTable::new();
    if let Some(v) = &info.title {
        t.push("Title", v.clone());
    }
    if let Some(v) = &info.title_id {
        t.push("Title ID", v.clone());
    }
    t.push("Content ID", info.content_id.clone());
    t.push(
        "Content type",
        match &info.content_type_label {
            Some(label) => format!("{} ({})", label, info.content_type),
            None => format!("{}", info.content_type),
        },
    );
    if let Some(v) = &info.category {
        t.push("Category", v.clone());
    }
    order_rom(&mut t);

    let mut out = String::new();
    section(&mut out, "Container", &c);
    section(&mut out, "ROM", &t);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_field(out: &str, key: &str, value: &str) -> bool {
        out.lines()
            .any(|l| l.trim_start().starts_with(&format!("{key}:")) && l.contains(value))
    }

    #[test]
    fn table_aligns_to_longest_key() {
        let mut t = KeyValueTable::new();
        t.push("Short", "1").push("Much longer key", "2");
        let out = t.render();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        // Both colons should be at the same column.
        let col1 = lines[0].find(':').unwrap();
        let col2 = lines[1].find(':').unwrap();
        assert_eq!(col1, "Short".len());
        assert_eq!(col2, "Much longer key".len());
    }

    #[test]
    fn render_chd_writes_format_line() {
        let info = rom_converto_lib::info::ChdInfo {
            version: 5,
            physical_bytes: 42,
            ..Default::default()
        };
        let out = render_chd(&info);
        assert!(has_field(&out, "Format", "CHD v5"));
        assert!(has_field(&out, "Physical bytes", "42"));
    }

    #[test]
    fn render_chd_writes_laserdisc_block() {
        use rom_converto_lib::chd::info::{ChdLdInfo, ChdLdVbiInfo, LdClvTime, LdDiscType};

        let info = rom_converto_lib::info::ChdInfo {
            version: 5,
            ld: Some(ChdLdInfo {
                fps: "59.940058".to_string(),
                width: 720,
                height: 240,
                interlaced: true,
                channels: 2,
                sample_rate: 48000,
                frame_count: 1000,
                vbi: Some(ChdLdVbiInfo {
                    disc_type: LdDiscType::Clv,
                    white_flag_count: 12,
                    clv_start_time: Some(LdClvTime {
                        hours: 0,
                        minutes: 5,
                    }),
                    clv_end_time: Some(LdClvTime {
                        hours: 1,
                        minutes: 10,
                    }),
                    chapter_min: Some(1),
                    chapter_max: Some(9),
                    lead_in: true,
                    lead_out: false,
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let out = render_chd(&info);
        assert!(out.contains("LaserDisc:"));
        assert!(out.contains("59.940058"));
        assert!(out.contains("720x240"));
        assert!(out.contains("VBI:"));
        assert!(out.contains("CLV"));
        assert!(out.contains("0:05-1:10"));
        assert!(out.contains("Chapters"));
        assert!(out.contains("1-9"));
    }

    fn xiso_info(
        xbe: Option<rom_converto_lib::microsoft::xbox::XbeInfo>,
        xex: Option<XexInfo>,
    ) -> rom_converto_lib::info::XisoInfo {
        rom_converto_lib::info::XisoInfo {
            kind: rom_converto_lib::microsoft::xdvdfs::PartitionKind::Xgd2,
            base: 0,
            root_sector: 0,
            root_size: 0,
            file_count: 1,
            dir_count: 1,
            total_file_bytes: 100,
            image_size: 200,
            xbe,
            xex,
            root_entries: Vec::new(),
        }
    }

    fn xbe_info() -> rom_converto_lib::microsoft::xbox::XbeInfo {
        rom_converto_lib::microsoft::xbox::XbeInfo {
            title_id: 0x4D530539,
            title_id_hex: "4D530539".to_string(),
            title_id_code: "MS-1337".to_string(),
            title_name: "Test Game".to_string(),
            alternate_title_ids: vec![0x11223344],
            allowed_media: 1,
            allowed_media_names: vec!["Hard Disk".to_string()],
            region: 1,
            region_names: vec!["North America".to_string()],
            ratings: 0,
            disc_number: 1,
            version: 1,
            cert_timestamp: 0,
            icon: Some(rom_converto_lib::info::Image::new(
                vec![0x89, b'P', b'N', b'G'],
                128,
                128,
            )),
        }
    }

    fn xex_info() -> XexInfo {
        XexInfo {
            title_id: 0x4D5307E6,
            title_id_hex: "4D5307E6".to_string(),
            media_id: 0x12345678,
            version: "1.0.0.0".to_string(),
            version_raw: 0,
            base_version: "1.0.0.0".to_string(),
            base_version_raw: 0,
            disc_number: 1,
            disc_count: 1,
            platform: 0,
            original_pe_name: Some("default.exe".to_string()),
            region: 0xFFFFFFFF,
            region_names: vec!["World".to_string()],
            allowed_media: 0xFF,
            title_name: Some("Test Xenon Game".to_string()),
            icon: None,
        }
    }

    #[test]
    fn render_xbox_shows_xbe_section_when_present() {
        let info = xiso_info(Some(xbe_info()), None);
        let out = render_xbox(&info);
        assert!(has_field(&out, "Title name", "Test Game"));
        assert!(has_field(&out, "Title ID", "4D530539 (MS-1337)"));
        assert!(has_field(&out, "Region", "North America"));
        assert!(has_field(&out, "Allowed media", "Hard Disk"));
        assert!(has_field(&out, "Ratings", "0x00000000"));
        assert!(has_field(&out, "Cert timestamp", "0"));
        assert!(has_field(&out, "Alternate title IDs", "11223344"));
        assert!(has_field(&out, "Icon", "128x128 PNG (4 bytes)"));
    }

    #[test]
    fn render_xbox_omits_xbe_section_when_absent() {
        let info = xiso_info(None, None);
        let out = render_xbox(&info);
        assert!(!out.contains("Title name:"));
        assert!(!out.contains("Alternate title IDs:"));
    }

    #[test]
    fn render_xbox_shows_xex_section_when_present() {
        let info = xiso_info(None, Some(xex_info()));
        let out = render_xbox(&info);
        assert!(has_field(&out, "Title name", "Test Xenon Game"));
        assert!(has_field(&out, "Media ID", "12345678"));
        assert!(has_field(&out, "Disc", "1/1"));
    }

    #[test]
    fn render_xbox_does_not_duplicate_shared_keys_when_xbe_and_xex_present() {
        let info = xiso_info(Some(xbe_info()), Some(xex_info()));
        let out = render_xbox(&info);
        let count = |key: &str| {
            out.lines()
                .filter(|l| l.trim_start().starts_with(&format!("{key}:")))
                .count()
        };
        assert_eq!(count("Title ID"), 1);
        assert_eq!(count("Version"), 1);
        assert_eq!(count("Region"), 1);
        assert_eq!(count("Icon"), 1);
        assert!(has_field(&out, "Media ID", "12345678"));
    }

    #[test]
    fn render_xbox_omits_rom_section_when_no_xbe_or_xex() {
        let info = xiso_info(None, None);
        let out = render_xbox(&info);
        assert!(!out.contains("ROM:"));
        assert!(!out.contains("Content type:"));
    }

    #[test]
    fn render_xenon_shows_xex_section_when_present() {
        let info = rom_converto_lib::info::ZarInfo {
            file_count: 1,
            dir_count: 1,
            logical_size: 100,
            compressed_size: 50,
            block_count: 1,
            has_default_xex: true,
            xex: Some(xex_info()),
            root_entries: Vec::new(),
        };
        let out = render_xenon(&info);
        assert!(has_field(&out, "Title name", "Test Xenon Game"));
        assert!(has_field(&out, "Original PE name", "default.exe"));
    }

    #[test]
    fn render_xenon_omits_xex_section_when_absent() {
        let info = rom_converto_lib::info::ZarInfo {
            file_count: 1,
            dir_count: 1,
            logical_size: 100,
            compressed_size: 50,
            block_count: 1,
            has_default_xex: false,
            xex: None,
            root_entries: Vec::new(),
        };
        let out = render_xenon(&info);
        assert!(!out.contains("Title name:"));
        assert!(!out.contains("ROM:"));
        assert!(!out.contains("Content type:"));
    }

    #[test]
    fn render_ctr_shows_ncsd_partitions_and_cia_contents() {
        let info = rom_converto_lib::info::CtrInfo {
            ncsd_partitions: vec![rom_converto_lib::nintendo::ctr::info::CtrPartitionEntry {
                index: 0,
                name: "Main".to_string(),
                offset: 0x4000,
                size: 0x1000,
            }],
            cia_contents: vec![rom_converto_lib::nintendo::ctr::info::CtrContentEntry {
                index: 0,
                content_id: "00000000".to_string(),
                size: 100,
                encrypted: true,
            }],
            ..Default::default()
        };
        let out = render_ctr(&info);
        assert!(out.contains("0  Main  4096 bytes @ 0x4000"));
        assert!(out.contains("0  00000000  100 bytes  encrypted"));
    }

    #[test]
    fn render_dol_shows_fst_root_and_totals_when_truncated() {
        let info = rom_converto_lib::info::DolInfo {
            fst_root: vec![
                rom_converto_lib::nintendo::dol::info::DolFstEntry {
                    name: "boot.dol".to_string(),
                    size: 123,
                    is_dir: false,
                },
                rom_converto_lib::nintendo::dol::info::DolFstEntry {
                    name: "files".to_string(),
                    size: 0,
                    is_dir: true,
                },
            ],
            fst_file_count: 5,
            fst_dir_count: 2,
            ..Default::default()
        };
        let out = render_dol(&info);
        assert!(out.contains("boot.dol  123 bytes"));
        assert!(out.contains("files/"));
        assert!(out.contains("5 files, 2 dirs"));
    }

    #[test]
    fn render_wup_shows_disc_partitions() {
        let info = rom_converto_lib::info::WupInfo {
            disc_partitions: vec![rom_converto_lib::nintendo::wup::info::WupDiscPartition {
                name: "GM_DISC".to_string(),
                kind: "Game".to_string(),
                start_sector: 100,
            }],
            ..Default::default()
        };
        let out = render_wup(&info);
        assert!(out.contains("GM_DISC  Game  sector 100"));
    }

    #[test]
    fn wup_encryption_derives_from_source_kind() {
        assert_eq!(wup_encryption("disc (Test Game)"), Some("encrypted"));
        assert_eq!(wup_encryption("nus"), Some("encrypted"));
        assert_eq!(wup_encryption("loadiine"), Some("decrypted"));
        assert_eq!(wup_encryption("wua (Test Game)"), Some("decrypted"));
        assert_eq!(wup_encryption("something else"), None);
    }

    #[test]
    fn render_wup_shows_encryption_row() {
        let info = rom_converto_lib::info::WupInfo {
            source_kind: "disc (Test Game)".to_string(),
            ..Default::default()
        };
        let out = render_wup(&info);
        assert!(has_field(&out, "Encryption", "encrypted"));
    }

    #[test]
    fn render_ps3_shows_icon_and_root_files() {
        let info = rom_converto_lib::info::Ps3Info {
            icon: Some(rom_converto_lib::info::Image {
                png_bytes: vec![0u8; 10],
                width: 128,
                height: 128,
            }),
            root_files: vec![rom_converto_lib::ps3::info::Ps3RootEntry {
                name: "PS3_GAME".to_string(),
                size: 0,
                is_dir: true,
            }],
            encrypted: Some(false),
            ..Default::default()
        };
        let out = render_ps3(&info);
        assert!(has_field(&out, "Icon", "128x128 PNG (10 bytes)"));
        assert!(has_field(&out, "Encryption", "decrypted"));
        assert!(out.contains("PS3_GAME/"));
    }

    #[test]
    fn render_ps3_omits_the_encryption_row_when_undetermined() {
        let info = rom_converto_lib::info::Ps3Info::default();
        let out = render_ps3(&info);
        assert!(!out.contains("Encryption"));
    }

    #[test]
    fn render_nx_shows_encryption_row() {
        let info = rom_converto_lib::info::NxInfo::default();
        let out = render_nx(&info);
        assert!(has_field(&out, "Encryption", "encrypted (standard keys)"));

        let info = rom_converto_lib::info::NxInfo {
            tickets: vec![rom_converto_lib::nintendo::nx::info::TicketSummary {
                file_name: "01020304.tik".to_string(),
                rights_id: "deadbeef".to_string(),
                master_key_revision: 0,
            }],
            ..Default::default()
        };
        let out = render_nx(&info);
        assert!(has_field(&out, "Encryption", "encrypted (titlekey)"));
    }

    #[test]
    fn render_nx_reports_ncz_containers_as_decrypted() {
        use rom_converto_lib::nintendo::nx::info::NxContainerKind;

        for kind in [NxContainerKind::Nsz, NxContainerKind::Xcz] {
            let info = rom_converto_lib::info::NxInfo {
                container_kind: kind,
                tickets: vec![rom_converto_lib::nintendo::nx::info::TicketSummary {
                    file_name: "01020304.tik".to_string(),
                    rights_id: "deadbeef".to_string(),
                    master_key_revision: 0,
                }],
                ..Default::default()
            };
            let out = render_nx(&info);
            assert!(has_field(&out, "Encryption", "decrypted (ncz sections)"));
        }

        let info = rom_converto_lib::info::NxInfo {
            container_kind: NxContainerKind::Xci,
            ..Default::default()
        };
        let out = render_nx(&info);
        assert!(has_field(&out, "Encryption", "encrypted (standard keys)"));
    }

    #[test]
    fn render_psp_shows_icon_and_background() {
        let info = rom_converto_lib::info::PspInfo {
            icon: Some(rom_converto_lib::info::Image {
                png_bytes: vec![0u8; 10],
                width: 144,
                height: 80,
            }),
            background: Some(rom_converto_lib::info::Image {
                png_bytes: vec![0u8; 20],
                width: 480,
                height: 272,
            }),
            ..Default::default()
        };
        let out = render_psp(&info);
        assert!(has_field(&out, "Icon", "144x80 PNG (10 bytes)"));
        assert!(has_field(&out, "Background", "480x272 PNG (20 bytes)"));
    }

    #[test]
    fn render_chd_appends_pgtype_and_pgsub_to_track_line() {
        let info = rom_converto_lib::info::ChdInfo {
            version: 5,
            tracks: vec![rom_converto_lib::chd::info::ChdTrack {
                number: 1,
                track_type: "MODE1/2048".to_string(),
                frames: 100,
                pregap: 0,
                subtype: None,
                pgtype: Some("MODE1".to_string()),
                pgsub: Some("RW".to_string()),
                postgap: None,
            }],
            ..Default::default()
        };
        let out = render_chd(&info);
        assert!(out.contains("pgtype=MODE1"));
        assert!(out.contains("pgsub=RW"));
    }

    #[test]
    fn render_rvl_shows_system_version() {
        let info = rom_converto_lib::info::RvlInfo {
            tmd: Some(rom_converto_lib::nintendo::rvl::info::RvlTmdInfo {
                title_id: 0x0001000012345678,
                title_id_hex: "0001000012345678".to_string(),
                title_version: 1,
                system_version: 0x0000000100000021,
                ios_slot: None,
                region_name: "NTSC-U".to_string(),
                content_count: 1,
                access_rights: 0,
            }),
            ..Default::default()
        };
        let out = render_rvl(&info);
        assert!(has_field(&out, "System version", "0000000100000021"));
    }

    #[test]
    fn render_nds_shows_secure_area_crc_and_banner_titles() {
        use rom_converto_lib::info::{LanguageCode, MultilingualString};
        use rom_converto_lib::nintendo::nds::info::{NdsBannerInfo, NdsSecureAreaState};

        let info = rom_converto_lib::info::NdsInfo {
            game_code: "ARCE".to_string(),
            secure_area: NdsSecureAreaState::Encrypted,
            header_crc16: 0x1234,
            header_crc16_computed: 0x1234,
            header_crc16_valid: true,
            banner: Some(NdsBannerInfo {
                banner_version: 1,
                titles: MultilingualString::from_pairs([(
                    LanguageCode::English,
                    "Test Game".to_string(),
                )]),
                banner_crc16: 0xABCD,
                banner_crc16_computed: 0xABCD,
                banner_crc16_valid: true,
                icon: rom_converto_lib::info::Image {
                    png_bytes: vec![0u8; 10],
                    width: 32,
                    height: 32,
                },
            }),
            ..Default::default()
        };
        let out = render_nds(&info);
        assert!(has_field(&out, "Secure area", "encrypted"));
        assert!(has_field(
            &out,
            "Header CRC16",
            "0x1234 (computed 0x1234, valid)"
        ));
        assert!(has_field(&out, "Title", "Test Game"));
        assert!(has_field(&out, "Icon", "32x32 PNG (10 bytes)"));
        assert!(out.contains("Banner titles:"));
    }

    #[test]
    fn render_retro_shows_checksum_lines() {
        use rom_converto_lib::retro::{GbaInfo, RetroDetails};

        let info = rom_converto_lib::info::RetroInfo {
            file_size: 4096,
            details: RetroDetails::Gba(GbaInfo {
                title: "TEST GAME".to_string(),
                game_code: "AXVE".to_string(),
                region: Some("Europe".to_string()),
                maker_code: "01".to_string(),
                version: 1,
                header_checksum: 0x5A,
                computed_header_checksum: 0x5B,
                header_checksum_valid: false,
                logo_valid: true,
            }),
        };
        let out = render_retro(&info);
        assert!(has_field(&out, "Format", "Game Boy Advance"));
        assert!(has_field(&out, "Title", "TEST GAME"));
        assert!(has_field(&out, "Checksum", "0x5A"));
        assert!(has_field(&out, "Computed checksum", "0x5B"));
        assert!(has_field(&out, "Checksum valid", "no"));
        assert!(has_field(&out, "Size", "4096"));
    }

    #[test]
    fn render_pbp_states_npumdimg_is_encrypted_and_lists_segments() {
        use rom_converto_lib::sony::psp::{PbpSegmentInfo, PsarKind};

        let info = rom_converto_lib::info::PbpInfo {
            physical_bytes: 1024,
            version: 0x10000,
            title: Some("Test EBOOT".to_string()),
            disc_id: Some("NPUH10041".to_string()),
            disc_version: Some("1.02".to_string()),
            category: Some("UG".to_string()),
            category_label: Some("UMD game".to_string()),
            psp_system_ver: Some("6.20".to_string()),
            parental_level: Some(5),
            region: Some(0x8000),
            icon: None,
            segments: vec![
                PbpSegmentInfo {
                    name: "PARAM.SFO".to_string(),
                    offset: 0x28,
                    size: 100,
                    present: true,
                },
                PbpSegmentInfo {
                    name: "PIC0.PNG".to_string(),
                    offset: 0x8C,
                    size: 0,
                    present: false,
                },
            ],
            psar_kind: Some(PsarKind::Npumdimg),
        };
        let out = render_pbp(&info);
        assert!(has_field(
            &out,
            "DATA.PSAR",
            "NPUMDIMG (encrypted PSN UMD image)"
        ));
        assert!(has_field(&out, "Content type", "UMD game (UG)"));
        assert!(out.contains("PARAM.SFO"));
        assert!(out.contains("absent"));
    }

    #[test]
    fn render_vpk_shows_category_label_and_totals() {
        let info = rom_converto_lib::info::VpkInfo {
            title: Some("Example Game".to_string()),
            title_id: Some("PCSF00001".to_string()),
            category: Some("gd".to_string()),
            category_label: Some("Application".to_string()),
            file_count: 3,
            total_size: 4096,
            ..Default::default()
        };
        let out = render_vpk(&info);
        assert!(has_field(&out, "Content type", "Application (gd)"));
        assert!(has_field(&out, "Files", "3"));
        assert!(has_field(&out, "Total size", "4096 bytes"));
    }

    #[test]
    fn render_pkg_shows_content_type_and_key_type() {
        let info = rom_converto_lib::info::PkgInfo {
            content_id: "EP9000-PCSF00001_00-EXAMPLE000000000".to_string(),
            content_type_label: Some("PS Vita application".to_string()),
            content_type: 0x15,
            key_type: 2,
            item_count: 4,
            ..Default::default()
        };
        let out = render_pkg(&info);
        assert!(has_field(&out, "Content type", "PS Vita application (21)"));
        assert!(has_field(&out, "Key type", "2"));
        assert!(has_field(
            &out,
            "Content ID",
            "EP9000-PCSF00001_00-EXAMPLE000000000"
        ));
    }
}

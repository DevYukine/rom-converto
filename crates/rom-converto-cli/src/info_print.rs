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
}

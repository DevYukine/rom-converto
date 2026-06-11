#![allow(dead_code)]

use anyhow::Result;
use rom_converto_lib::info::InfoResult;
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
    };
    print!("{}", rendered);
    Ok(())
}

fn render_cso(info: &rom_converto_lib::info::CsoInfo) -> String {
    let mut t = KeyValueTable::new();
    t.push("Format", format!("{} v{}", info.format, info.version));
    t.push("Block size", format!("{} bytes", info.block_size));
    t.push("Index shift", format!("{}", info.index_shift));
    t.push(
        "Blocks",
        format!("{} ({} stored raw)", info.block_count, info.raw_block_count),
    );
    t.push("Uncompressed bytes", format!("{}", info.uncompressed_size));
    t.push("Physical bytes", format!("{}", info.physical_bytes));
    t.push(
        "Compression ratio",
        format!("{:.2}%", info.compression_ratio),
    );
    t.render()
}

fn render_chd(info: &rom_converto_lib::info::ChdInfo) -> String {
    let mut t = KeyValueTable::new();
    t.push("Format", format!("CHD v{}", info.version));
    if info.compressors.is_empty() {
        t.push("Compressors", "(none)");
    } else {
        t.push("Compressors", info.compressors.join(", "));
    }
    t.push("Hunk size", format!("{} bytes", info.hunk_bytes));
    t.push("Unit size", format!("{} bytes", info.unit_bytes));
    t.push("Hunks", format!("{}", info.hunk_count));
    t.push("Logical bytes", format!("{}", info.logical_bytes));
    t.push("Physical bytes", format!("{}", info.physical_bytes));
    t.push(
        "Compression ratio",
        format!("{:.2}%", info.compression_ratio),
    );
    t.push("Raw SHA1", info.raw_sha1.clone());
    t.push("SHA1", info.sha1.clone());
    if let Some(parent) = &info.parent_sha1 {
        t.push("Parent SHA1", parent.clone());
    }
    if let Some(vers) = &info.version_string {
        t.push("chdman version", vers.clone());
    }
    if let Some(dvd) = &info.dvd {
        let layer = match dvd.layer_class {
            rom_converto_lib::chd::info::DvdLayerClass::SingleLayer => "single-layer (4.7 GB)",
            rom_converto_lib::chd::info::DvdLayerClass::DualLayer => "dual-layer (8.5 GB)",
        };
        t.push(
            "DVD geometry",
            format!("{} sectors, {}", dvd.total_sectors, layer),
        );
    }
    let mut out = t.render();

    if !info.tracks.is_empty() {
        out.push_str("\nTracks:\n");
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
            out.push_str(&format!(
                "  {:>2}  {:<12}  frames={:<8} pregap={}{}{}\n",
                tr.number, tr.track_type, tr.frames, tr.pregap, subtype, postgap
            ));
        }
    }

    if !info.metadata_tags.is_empty() {
        out.push_str("\nMetadata tags:\n");
        for tag in &info.metadata_tags {
            out.push_str(&format!("  {}  ({} bytes)\n", tag.tag, tag.length));
        }
    }

    out
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
    t.push("Program ID", info.program_id.clone());
    t.push("Product code", info.product_code.clone());
    t.push(
        "Maker code",
        format_maker(&info.maker_code, info.maker_name.as_deref()),
    );
    if let Some(sz) = info.cartridge_size {
        t.push("Cartridge size", format!("{} bytes", sz));
    }
    t.push("Physical bytes", format!("{}", info.physical_bytes));
    t.push(
        "NCCH encrypted",
        if info.ncch_encrypted { "yes" } else { "no" },
    );
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
    let mut out = t.render();

    if let Some(s) = &info.smdh {
        if !s.region_names.is_empty() {
            out.push_str(&format!("\nRegion: {}\n", s.region_names.join(", ")));
        }
        out.push_str(&format!("Flags: 0x{:08X}\n", s.flags));

        if !s.titles.is_empty() {
            out.push_str("\nTitles:\n");
            for t in &s.titles {
                out.push_str(&format!(
                    "  {:<22}  {} ({})\n",
                    t.language,
                    t.long_description.replace('\n', " "),
                    t.publisher
                ));
            }
        }
        if !s.age_ratings.is_empty() {
            out.push_str("\nAge ratings:\n");
            for r in &s.age_ratings {
                let banned = if r.banned { " banned" } else { "" };
                let pending = if r.pending { " pending" } else { "" };
                out.push_str(&format!(
                    "  {:<10}  age {}{}{}\n",
                    r.region, r.age, banned, pending
                ));
            }
        }
    }

    if let Some(img) = &info.icon {
        out.push_str(&format!(
            "\nIcon: {}x{} PNG ({} bytes)\n",
            img.width,
            img.height,
            img.png_bytes.len()
        ));
    }

    out
}

fn render_dol(info: &rom_converto_lib::info::DolInfo) -> String {
    let mut t = KeyValueTable::new();
    t.push("Format", "GameCube");
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
    let mut out = t.render();

    if let Some(banner) = &info.banner {
        out.push_str(&format!("\nBanner format: {}\n", banner.format));
        if !banner.titles.is_empty() {
            out.push_str("\nBanner titles:\n");
            for t in &banner.titles {
                out.push_str(&format!(
                    "  {:<10}  {} ({})\n    {}\n",
                    t.language,
                    t.long_game_name,
                    t.long_maker,
                    t.description.replace('\n', " ")
                ));
            }
        }
    }

    if let Some(img) = &info.banner_image {
        out.push_str(&format!(
            "\nBanner image: {}x{} PNG ({} bytes)\n",
            img.width,
            img.height,
            img.png_bytes.len()
        ));
    }

    out
}

fn render_rvl(info: &rom_converto_lib::info::RvlInfo) -> String {
    let mut t = KeyValueTable::new();
    t.push("Format", "Wii");
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
        if let Some(ios) = tmd.ios_slot {
            t.push("IOS slot", format!("IOS{}", ios));
        }
        t.push("TMD region", tmd.region_name.clone());
        t.push("Content count", format!("{}", tmd.content_count));
        t.push("Access rights", format!("0x{:08X}", tmd.access_rights));
    }
    let mut out = t.render();

    if !info.partitions.is_empty() {
        out.push_str("\nPartitions:\n");
        for p in &info.partitions {
            out.push_str(&format!(
                "  group={} type={} ({:<7})  offset=0x{:X}\n",
                p.group, p.partition_type, p.kind, p.offset
            ));
        }
    }

    if let Some(names) = &info.imet_names
        && !names.is_empty()
    {
        out.push_str("\nIMET banner names:\n");
        for (lang, name) in &names.entries {
            out.push_str(&format!("  {:<10?}  {}\n", lang, name));
        }
    }

    out
}

fn render_wup(info: &rom_converto_lib::info::WupInfo) -> String {
    let mut t = KeyValueTable::new();
    t.push("Format", format!("Wii U ({})", info.source_kind));
    t.push("Title ID", info.title_id_hex.clone());
    t.push("Title type", info.title_type.clone());
    if let Some(uv) = info.update_version {
        t.push(
            "Title version",
            format!("v{} (base v{})", uv, info.title_version),
        );
    } else {
        t.push("Title version", format!("v{}", info.title_version));
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
    let mut out = t.render();

    if !info.bundled_titles.is_empty() {
        out.push_str("\nBundled titles:\n");
        for bt in &info.bundled_titles {
            out.push_str(&format!(
                "  {}  {:<8}  v{}\n",
                bt.title_id_hex, bt.title_type, bt.title_version
            ));
        }
    }

    if let Some(meta) = &info.meta {
        if !meta.region_names.is_empty() {
            out.push_str(&format!("\nRegion: {}\n", meta.region_names.join(", ")));
        }
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
        if !meta.long_names.is_empty() {
            out.push_str("\nLong names:\n");
            for (lang, name) in &meta.long_names.entries {
                out.push_str(&format!("  {:<22?}  {}\n", lang, name));
            }
        }
        if !meta.publishers.is_empty() {
            out.push_str("\nPublishers:\n");
            for (lang, name) in &meta.publishers.entries {
                out.push_str(&format!("  {:<22?}  {}\n", lang, name));
            }
        }
        if !meta.age_ratings.is_empty() {
            out.push_str("\nAge ratings:\n");
            let mut keys: Vec<&String> = meta.age_ratings.keys().collect();
            keys.sort();
            for k in keys {
                out.push_str(&format!("  {:<10}  {}\n", k, meta.age_ratings[k]));
            }
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

    let mut t = KeyValueTable::new();
    t.push("Format", format!("Switch {}", kind_str));
    t.push(
        "Compressed",
        if info.is_compressed {
            "yes (zstd)"
        } else {
            "no"
        },
    );
    t.push("Distribution", info.distribution.display_name());
    t.push("Structure", info.structure.display_name());
    t.push("Physical bytes", format!("{}", info.physical_bytes));
    t.push("Files", format!("{}", info.files.len()));
    t.push("NCA files", format!("{}", info.nca_names.len()));
    t.push("CNMT NCAs", format!("{}", info.cnmt_nca_names.len()));
    t.push("Tickets", format!("{}", info.tickets.len()));
    if let Some(full) = &info.full {
        t.push("Title ID", format!("{:016X}", full.application_title_id));
        t.push("Title kind", full.title_kind.display_name());
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
    let mut out = t.render();

    if let Some(parts) = &info.xci_partitions {
        out.push_str("\nXCI partitions:\n");
        for p in parts {
            out.push_str(&format!(
                "  {:<8} {} files, {} bytes\n",
                p.name, p.file_count, p.total_size
            ));
        }
    }

    if !info.tickets.is_empty() {
        out.push_str("\nTickets:\n");
        for tk in &info.tickets {
            out.push_str(&format!(
                "  {:<40}  rights_id={}  master_key_rev={}\n",
                tk.file_name, tk.rights_id, tk.master_key_revision
            ));
        }
    }

    if !info.cnmt_nca_names.is_empty() {
        out.push_str("\nCNMT NCAs:\n");
        for n in &info.cnmt_nca_names {
            out.push_str(&format!("  {}\n", n));
        }
    }

    if let Some(full) = &info.full {
        if !full.contents.is_empty() {
            out.push_str("\nCNMT contents:\n");
            for c in &full.contents {
                out.push_str(&format!(
                    "  {:<10}  {:>12} bytes  id={}\n",
                    c.content_type, c.size, c.content_id
                ));
            }
        }
        if !full.related_titles.is_empty() {
            out.push_str("\nRelated titles:\n");
            for r in &full.related_titles {
                out.push_str(&format!(
                    "  {:016X}  {:<14}  v{}\n",
                    r.title_id,
                    r.kind.display_name(),
                    r.version
                ));
            }
        }
        if let Some(ctrl) = &full.control {
            out.push_str(&format!("\nDisplay version: {}\n", ctrl.display_version));
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
            if !ctrl.age_ratings.is_empty() {
                out.push_str("\nAge ratings:\n");
                for r in &ctrl.age_ratings {
                    out.push_str(&format!("  {:<14}  {}\n", r.organization, r.age));
                }
            }
            if !ctrl.titles.is_empty() {
                out.push_str("\nTitles:\n");
                for t in &ctrl.titles {
                    out.push_str(&format!(
                        "  {:<22}  {}  ({})\n",
                        t.language, t.name, t.publisher
                    ));
                }
            }
            out.push_str("\nSave data sizes (bytes):\n");
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
            if let Some(lang) = &ctrl.icon_language
                && let Some(img) = &ctrl.icon
            {
                out.push_str(&format!(
                    "\nIcon: {}x{} PNG ({} bytes, language {})\n",
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(out.contains("Format:"));
        assert!(out.contains("CHD v5"));
        assert!(out.contains("42"));
    }
}

#![allow(dead_code)]

use anyhow::Result;
use rom_converto_lib::info::InfoResult;
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
    t.push("Format", format!("GameCube ({})", info.container));
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
    t.push("Format", format!("Wii ({})", info.container));
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

fn render_xbox(info: &rom_converto_lib::info::XisoInfo) -> String {
    use rom_converto_lib::microsoft::xdvdfs::PartitionKind;
    let kind = match info.kind {
        PartitionKind::Trimmed => "trimmed".to_string(),
        PartitionKind::Xgd1 => "XGD1".to_string(),
        PartitionKind::Xgd2 => "XGD2".to_string(),
        PartitionKind::Xgd3 => "XGD3".to_string(),
        PartitionKind::X360Extra(base) => format!("X360 extra (base 0x{:X})", base),
    };

    let mut t = KeyValueTable::new();
    t.push("Format", "Xbox XISO");
    t.push("Partition kind", kind);
    t.push("Base offset", format!("0x{:X}", info.base));
    t.push("Root sector", format!("{}", info.root_sector));
    t.push("Root size", format!("{} bytes", info.root_size));
    t.push("Files", format!("{}", info.file_count));
    t.push("Directories", format!("{}", info.dir_count));
    t.push("Total file bytes", format!("{}", info.total_file_bytes));
    t.push("Image size", format!("{} bytes", info.image_size));
    let mut out = t.render();

    if let Some(xbe) = &info.xbe {
        out.push('\n');
        out.push_str(&format!("Title name: {}\n", xbe.title_name));
        out.push_str(&format!(
            "Title ID: {} ({})\n",
            xbe.title_id_hex, xbe.title_id_code
        ));
        out.push_str(&format!("Version: {}\n", xbe.version));
        out.push_str(&format!("Disc number: {}\n", xbe.disc_number));
        out.push_str(&format!(
            "Region: {}\n",
            if xbe.region_names.is_empty() {
                format!("0x{:08X}", xbe.region)
            } else {
                xbe.region_names.join(", ")
            }
        ));
        out.push_str(&format!(
            "Allowed media: {}\n",
            xbe.allowed_media_names.join(", ")
        ));
        out.push_str(&format!("Ratings: 0x{:08X}\n", xbe.ratings));
        out.push_str(&format!("Cert timestamp: {}\n", xbe.cert_timestamp));
        if !xbe.alternate_title_ids.is_empty() {
            out.push_str(&format!(
                "Alternate title IDs: {}\n",
                xbe.alternate_title_ids
                    .iter()
                    .map(|id| format!("{:08X}", id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    if let Some(xex) = &info.xex {
        render_xex_section(&mut out, xex);
    }

    out
}

fn render_ps3(info: &rom_converto_lib::info::Ps3Info) -> String {
    let mut t = KeyValueTable::new();
    t.push("Format", "PS3 ISO");
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
    t.render()
}

fn render_xenon(info: &rom_converto_lib::info::ZarInfo) -> String {
    let mut t = KeyValueTable::new();
    t.push("Format", "Xbox 360 ZArchive");
    t.push("Files", format!("{}", info.file_count));
    t.push("Directories", format!("{}", info.dir_count));
    t.push("Logical bytes", format!("{}", info.logical_size));
    t.push("Compressed bytes", format!("{}", info.compressed_size));
    t.push("Blocks", format!("{}", info.block_count));
    t.push(
        "default.xex",
        if info.has_default_xex {
            "present"
        } else {
            "not found"
        },
    );
    let mut out = t.render();

    if let Some(xex) = &info.xex {
        render_xex_section(&mut out, xex);
    }

    out
}

fn render_xex_section(out: &mut String, xex: &XexInfo) {
    out.push('\n');
    if let Some(name) = &xex.title_name {
        out.push_str(&format!("Title name: {}\n", name));
    }
    out.push_str(&format!("Title ID: {}\n", xex.title_id_hex));
    out.push_str(&format!("Media ID: {:08X}\n", xex.media_id));
    out.push_str(&format!("Version: {}\n", xex.version));
    out.push_str(&format!("Base version: {}\n", xex.base_version));
    out.push_str(&format!("Disc: {}/{}\n", xex.disc_number, xex.disc_count));
    out.push_str(&format!("Platform: {}\n", xex.platform));
    if let Some(pe) = &xex.original_pe_name {
        out.push_str(&format!("Original PE name: {}\n", pe));
    }
    out.push_str(&format!("Region: {}\n", xex.region_names.join(", ")));
    out.push_str(&format!("Allowed media: 0x{:08X}\n", xex.allowed_media));
    if let Some(img) = &xex.icon {
        out.push_str(&format!(
            "Icon: {}x{} PNG ({} bytes)\n",
            img.width,
            img.height,
            img.png_bytes.len()
        ));
    }
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
        assert!(out.contains("Title name: Test Game"));
        assert!(out.contains("Title ID: 4D530539 (MS-1337)"));
        assert!(out.contains("Region: North America"));
        assert!(out.contains("Allowed media: Hard Disk"));
        assert!(out.contains("Ratings: 0x00000000"));
        assert!(out.contains("Cert timestamp: 0"));
        assert!(out.contains("Alternate title IDs: 11223344"));
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
        assert!(out.contains("Title name: Test Xenon Game"));
        assert!(out.contains("Media ID: 12345678"));
        assert!(out.contains("Disc: 1/1"));
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
        };
        let out = render_xenon(&info);
        assert!(out.contains("Title name: Test Xenon Game"));
        assert!(out.contains("Original PE name: default.exe"));
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
        };
        let out = render_xenon(&info);
        assert!(!out.contains("Title name:"));
    }
}

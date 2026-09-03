import { ageRatingDisplayName, contentTypeDisplayName, enumDisplayName, languageDisplayName } from "./display";
import type { ChdLdInfo, DiscContent, InfoResult, LdClvTime, XboxInfo } from "~/types/info";

export interface InspectField {
	label: string;
	value: string;
}

export interface InnerFile {
	name: string;
	detail: string;
}

export interface InspectView {
	container: InspectField[];
	rom: InspectField[];
	innerTitle: string;
	innerFiles: InnerFile[];
	hashes: InspectField[];
	contentType: string | null;
}

export function formatMaker(code: string, name: string | null): string {
	return name ? `${code} (${name})` : code;
}

// Language tags differ per format ("AmericanEnglish", "english", "american_english");
// normalize before comparing. Falls back to the first entry.
export function englishFirst<T>(items: T[] | undefined, lang: (item: T) => string): T | undefined {
	if (!items?.length) return undefined;
	for (const pref of ["americanenglish", "english", "britishenglish"]) {
		const hit = items.find((item) => lang(item).replace(/[_\s]/g, "").toLowerCase() === pref);
		if (hit) return hit;
	}
	return items[0];
}

function hex(n: number, width: number): string {
	return n.toString(16).padStart(width, "0").toUpperCase();
}

export function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	const units = ["KiB", "MiB", "GiB", "TiB"];
	let value = n / 1024;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	return `${value.toFixed(value >= 10 ? 0 : 1)} ${units[unit]}`;
}

export function formatXboxPartitionKind(pk: XboxInfo["partition_kind"]): string {
	if (typeof pk === "object") return `X360 Extra (+${pk.x360_extra})`;
	switch (pk) {
		case "trimmed":
			return "Trimmed";
		case "xgd1":
			return "XGD1";
		case "xgd2":
			return "XGD2";
		case "xgd3":
			return "XGD3";
	}
}

export function xenonRatio(logicalSize: number, compressedSize: number): number {
	return logicalSize > 0 ? (1 - compressedSize / logicalSize) * 100 : 0;
}

function add(list: InspectField[], label: string, value: string | number | null | undefined) {
	if (value === null || value === undefined || value === "") return;
	list.push({ label, value: String(value) });
}

// "disc"/"nus" sources ship encrypted; "loadiine"/"wua" are already decrypted extractions.
export function wupEncryption(sourceKind: string): string | null {
	if (sourceKind.startsWith("disc") || sourceKind.startsWith("nus")) return "encrypted";
	if (sourceKind.startsWith("loadiine") || sourceKind.startsWith("wua")) return "decrypted";
	return null;
}

const CTR_CONTENT_TYPES: Record<string, string> = {
	"00040000": "Game",
	"0004000E": "Update",
	"0004008C": "DLC",
	"00040010": "System",
	"00040030": "System",
};

function ldClvTime(t: LdClvTime): string {
	return `${t.hours}:${String(t.minutes).padStart(2, "0")}`;
}

function ldDiscTypeLabel(discType: "cav" | "clv" | "unknown"): string {
	return discType === "unknown" ? "unknown" : discType.toUpperCase();
}

function ldVbiSummary(vbi: NonNullable<ChdLdInfo["vbi"]>): string {
	const parts = [ldDiscTypeLabel(vbi.disc_type)];
	if (vbi.cav_picture_min != null && vbi.cav_picture_max != null) {
		parts.push(`pic ${vbi.cav_picture_min}-${vbi.cav_picture_max}`);
	}
	if (vbi.clv_start_time && vbi.clv_end_time) {
		parts.push(`${ldClvTime(vbi.clv_start_time)}-${ldClvTime(vbi.clv_end_time)}`);
	}
	if (vbi.chapter_min != null && vbi.chapter_max != null) {
		parts.push(`ch ${vbi.chapter_min}-${vbi.chapter_max}`);
	}
	parts.push(`${vbi.white_flag_count} white flags`);
	return parts.join(" · ");
}

function discContentRom(content: DiscContent): InspectField[] {
	const rom: InspectField[] = [];
	if (content.kind === "psx") {
		add(rom, "Title", content.volume_id);
		add(rom, "Title ID", content.title_id);
		add(rom, "Content Type", "Game");
		add(rom, "Version", content.version);
		add(rom, "Size", formatBytes(content.size_bytes));
		add(rom, "Disc Kind", content.disc_kind);
		add(rom, "Boot Executable", content.boot_executable);
		add(rom, "Total Sectors", content.total_sectors);
	} else {
		add(rom, "Title", content.title);
		add(rom, "Title ID", content.title_id);
		add(rom, "Content Type", content.category ? enumDisplayName(content.category) : "Game");
		add(rom, "Version", content.version);
		add(rom, "Size", formatBytes(content.size_bytes));
		add(rom, "Firmware", content.firmware);
		add(rom, "Total Sectors", content.total_sectors);
	}
	return rom;
}

function crcField(rom: InspectField[], label: string, stored: number, computed: number, valid: boolean, width: number) {
	add(rom, label, `0x${hex(stored, width)} (${valid ? "valid" : `invalid, computed 0x${hex(computed, width)}`})`);
}

export function buildInspectView(info: InfoResult): InspectView {
	const container: InspectField[] = [];
	let rom: InspectField[] = [];
	let innerTitle = "Inner Files";
	let innerFiles: InnerFile[] = [];
	const hashes: InspectField[] = [];

	switch (info.kind) {
		case "chd": {
			add(container, "Container", `CHD v${info.version}`);
			add(container, "Compression", info.compressors.join(", ") || "none");
			add(container, "Compressed Size", formatBytes(info.physical_bytes));
			add(container, "Logical Size", formatBytes(info.logical_bytes));
			add(container, "Ratio", `${info.compression_ratio.toFixed(1)}%`);
			add(container, "Hunk", `${formatBytes(info.hunk_bytes)} × ${info.hunk_count}`);
			add(container, "Unit", formatBytes(info.unit_bytes));
			if (info.dvd) add(container, "DVD", `${info.dvd.total_sectors} sectors · ${info.dvd.layer_class}`);
			if (info.ld) {
				add(container, "LD FPS", info.ld.fps);
				add(container, "LD Field Size", `${info.ld.width}x${info.ld.height}`);
				add(container, "LD Interlaced", info.ld.interlaced ? "yes" : "no");
				add(container, "LD Audio", `${info.ld.channels} ch · ${info.ld.sample_rate} Hz`);
				add(container, "LD Frames", info.ld.frame_count);
				if (info.ld.vbi) add(container, "LD VBI", ldVbiSummary(info.ld.vbi));
			}
			add(container, "Metadata", info.metadata_tags.map((t) => t.tag).join(", "));
			if (info.content) rom = discContentRom(info.content);
			innerTitle = "Tracks";
			innerFiles = info.tracks.map((t) => {
				let detail = `${t.track_type} · ${t.frames} frames`;
				if (t.pregap > 0) {
					detail += ` · pregap ${t.pregap}`;
					if (t.pgtype) detail += ` (${t.pgtype}${t.pgsub ? `/${t.pgsub}` : ""})`;
				}
				if (t.postgap) detail += ` · postgap ${t.postgap}`;
				if (t.subtype) detail += ` · sub ${t.subtype}`;
				return { name: `Track ${t.number}`, detail };
			});
			add(hashes, "Raw SHA-1", info.raw_sha1);
			add(hashes, "SHA-1", info.sha1);
			add(hashes, "Parent SHA-1", info.parent_sha1);
			break;
		}
		case "cso": {
			add(container, "Container", `${info.format} v${info.version}`);
			add(container, "Compressed Size", formatBytes(info.physical_bytes));
			add(container, "Logical Size", formatBytes(info.uncompressed_size));
			add(container, "Ratio", `${info.compression_ratio.toFixed(1)}%`);
			add(container, "Block Size", formatBytes(info.block_size));
			add(container, "Blocks", `${info.block_count} (${info.raw_block_count} raw)`);
			add(container, "Index Shift", info.index_shift);
			if (info.content) rom = discContentRom(info.content);
			break;
		}
		case "ctr": {
			if (info.compressed) {
				add(container, "Container", "Z3DS");
				add(container, "Compression", "zstd");
				add(container, "Compressed Size", formatBytes(info.physical_bytes));
			}
			const smdh = info.smdh;
			const title = englishFirst(smdh?.titles, (t) => t.language);
			add(rom, "Title", title?.long_description || info.product_code || info.title_id);
			add(rom, "Title ID", info.title_id);
			add(rom, "Content Type", CTR_CONTENT_TYPES[info.title_id.slice(0, 8).toUpperCase()] ?? info.format.toUpperCase());
			if (smdh) {
				add(rom, "Region", smdh.region_names.join(", "));
				add(rom, "Languages", smdh.titles.map((t) => languageDisplayName(t.language)).join(", "));
			}
			add(rom, "Publisher", title?.publisher || formatMaker(info.maker_code, info.maker_name));
			if (smdh) {
				add(
					rom,
					"Age Ratings",
					smdh.age_ratings
						.map(
							(r) =>
								`${ageRatingDisplayName(r.region)} ${r.age}+${r.pending ? " (pending)" : ""}${r.banned ? " (banned)" : ""}`,
						)
						.join(", "),
				);
			}
			add(rom, "Size", formatBytes(info.physical_bytes));
			add(rom, "Program ID", info.program_id);
			add(rom, "Product Code", info.product_code);
			add(rom, "Maker", formatMaker(info.maker_code, info.maker_name));
			if (info.cartridge_size) add(rom, "Cartridge", formatBytes(info.cartridge_size));
			add(rom, "Encryption", info.ncch_encrypted ? "encrypted" : "decrypted");
			if (smdh) {
				add(rom, "EULA", `v${smdh.eula_version_major}.${smdh.eula_version_minor}`);
				add(rom, "Flags", `0x${hex(smdh.flags, 8)}`);
			}
			if (info.ncsd_partitions.length) {
				innerTitle = "Partitions";
				innerFiles = info.ncsd_partitions.map((p) => ({
					name: p.name,
					detail: `${formatBytes(p.size)} · 0x${p.offset.toString(16).toUpperCase()}`,
				}));
			} else if (info.cia_contents.length) {
				innerTitle = "Contents";
				innerFiles = info.cia_contents.map((c) => ({
					name: `Content ${c.index}`,
					detail: `${c.content_id} · ${formatBytes(c.size)}${c.encrypted ? " · encrypted" : ""}`,
				}));
			}
			break;
		}
		case "dol": {
			if (info.container.toUpperCase() !== "ISO") {
				add(container, "Container", info.container.toUpperCase());
				add(container, "Compressed Size", formatBytes(info.physical_bytes));
			}
			const banner = englishFirst(info.banner?.titles, (b) => b.language);
			add(rom, "Title", banner?.long_game_name || banner?.short_game_name || info.game_name);
			add(rom, "Title ID", info.game_id);
			add(rom, "Content Type", "Game");
			add(rom, "Version", `v${info.disc_version}`);
			add(rom, "Region", info.region);
			add(rom, "Languages", info.banner?.titles.map((t) => languageDisplayName(t.language)).join(", "));
			add(rom, "Publisher", banner?.long_maker || formatMaker(info.maker_code, info.maker_name));
			add(rom, "Size", formatBytes(info.physical_bytes));
			add(rom, "Disc Number", info.disc_number);
			add(rom, "Apploader Date", info.apploader_date);
			add(rom, "Audio Streaming", info.audio_streaming ? "yes" : "no");
			innerTitle = "Disc Files";
			innerFiles = info.fst_root.map((e) => ({ name: e.name, detail: e.is_dir ? "dir" : formatBytes(e.size) }));
			if (info.fst_file_count + info.fst_dir_count > info.fst_root.length) {
				innerFiles.push({ name: `${info.fst_file_count} files, ${info.fst_dir_count} dirs`, detail: "" });
			}
			break;
		}
		case "rvl": {
			if (info.container.toUpperCase() !== "ISO") {
				add(container, "Container", info.container.toUpperCase());
				add(container, "Compressed Size", formatBytes(info.physical_bytes));
			}
			const tmd = info.tmd;
			add(rom, "Title", englishFirst(info.imet_names?.entries, (e) => e[0])?.[1] || info.game_name);
			add(rom, "Title ID", tmd ? tmd.title_id_hex : info.game_id);
			add(rom, "Content Type", "Game");
			add(rom, "Version", tmd ? `v${tmd.title_version}` : `v${info.disc_version}`);
			add(rom, "Region", info.region);
			add(rom, "Languages", info.imet_names?.entries.map((e) => languageDisplayName(e[0])).join(", "));
			add(rom, "Publisher", formatMaker(info.maker_code, info.maker_name));
			add(rom, "Size", formatBytes(info.physical_bytes));
			add(rom, "Game ID", info.game_id);
			add(rom, "Disc Number", info.disc_number);
			if (tmd) {
				if (tmd.ios_slot != null) add(rom, "IOS", `IOS${tmd.ios_slot}`);
				add(rom, "TMD Region", tmd.region_name);
				add(rom, "TMD Contents", tmd.content_count);
				add(rom, "System Version", `0x${tmd.system_version.toString(16).toUpperCase()}`);
				add(rom, "Access Rights", `0x${hex(tmd.access_rights, 8)}`);
			}
			innerTitle = "Partitions";
			innerFiles = info.partitions.map((p) => ({
				name: p.kind,
				detail: `type ${p.partition_type} · group ${p.group} · 0x${p.offset.toString(16).toUpperCase()}`,
			}));
			break;
		}
		case "wup": {
			const meta = info.meta;
			add(rom, "Title", englishFirst(meta?.long_names.entries, (e) => e[0])?.[1] || info.title_id_hex);
			add(rom, "Title ID", info.title_id_hex);
			add(rom, "Content Type", info.title_type);
			add(rom, "Encryption", wupEncryption(info.source_kind));
			add(
				rom,
				"Version",
				info.update_version != null ? `v${info.update_version} (base v${info.title_version})` : `v${info.title_version}`,
			);
			if (meta) {
				add(rom, "Region", meta.region_names.join(", "));
				add(rom, "Languages", meta.long_names.entries.map((e) => languageDisplayName(e[0])).join(", "));
				add(
					rom,
					"Publisher",
					englishFirst(meta.publishers.entries, (e) => e[0])?.[1] || meta.company_name || meta.company_code,
				);
				add(
					rom,
					"Age Ratings",
					Object.entries(meta.age_ratings)
						.sort(([a], [b]) => a.localeCompare(b))
						.map(([org, age]) => `${ageRatingDisplayName(org)} ${age}+`)
						.join(", "),
				);
			}
			add(rom, "Size", formatBytes(info.total_content_size));
			if (meta) add(rom, "Product Code", meta.product_code);
			if (info.content_count > 0) add(rom, "Contents", String(info.content_count));
			add(rom, "OS Version", info.os_version);
			if (meta) {
				add(rom, "Mastered", meta.mastering_date);
				if (meta.save_size) add(rom, "Save Size", formatBytes(meta.save_size));
			}
			add(rom, "SDK Version", info.sdk_version);
			add(rom, "Access Rights", `0x${hex(info.access_rights, 8)}`);
			add(rom, "Group ID", `0x${hex(info.group_id, 4)}`);
			if (meta) {
				if (meta.app_size) add(rom, "App Size", formatBytes(meta.app_size));
				add(
					rom,
					"Boss Storage",
					[meta.boss_size, meta.common_boss_size, meta.account_boss_size]
						.filter((n): n is number => !!n)
						.map((n) => formatBytes(n))
						.join(" · "),
				);
				if (meta.eula_version != null) add(rom, "EULA", `v${meta.eula_version}`);
				if (meta.drc_use != null) add(rom, "GamePad", meta.drc_use ? "yes" : "no");
				if (meta.e_manual != null) add(rom, "e-Manual", meta.e_manual ? "yes" : "no");
				if (meta.network_use != null || meta.online_account_use != null) {
					add(
						rom,
						"Network",
						[
							meta.network_use != null ? `use ${meta.network_use}` : "",
							meta.online_account_use != null ? `account ${meta.online_account_use}` : "",
						]
							.filter(Boolean)
							.join(" · "),
					);
				}
			}
			if (info.disc_partitions.length) {
				innerTitle = "Disc Partitions";
				innerFiles = info.disc_partitions.map((p) => ({
					name: p.name,
					detail: `${p.kind} · sector ${p.start_sector}`,
				}));
			} else {
				innerTitle = "Bundled Titles";
				innerFiles = info.bundled_titles.map((b) => ({
					name: b.title_type,
					detail: `${b.title_id_hex} · v${b.title_version}`,
				}));
			}
			break;
		}
		case "nx": {
			add(container, "Container", info.container_kind.toUpperCase());
			add(container, "Compression", info.is_compressed ? "zstd" : "none");
			add(container, "Compressed Size", formatBytes(info.physical_bytes));
			add(container, "Distribution", enumDisplayName(info.distribution));
			add(container, "Structure", enumDisplayName(info.structure));
			add(container, "NCAs", `${info.nca_names.length} (${info.cnmt_nca_names.length} meta)`);
			add(container, "Tickets", info.tickets.length);
			if (info.xci_partitions?.length) {
				add(
					container,
					"XCI Partitions",
					info.xci_partitions.map((p) => `${p.name} (${p.file_count} files, ${formatBytes(p.total_size)})`).join("; "),
				);
			}
			const full = info.full;
			const ctrl = full?.control;
			const title = englishFirst(ctrl?.titles, (t) => t.language);
			add(rom, "Title", title?.name || info.container_kind.toUpperCase());
			if (full) {
				add(rom, "Title ID", full.application_title_id_hex);
				add(rom, "Content Type", contentTypeDisplayName(full.title_kind));
				add(
					rom,
					"Version",
					ctrl?.display_version ? `${ctrl.display_version} (v${full.title_version})` : `v${full.title_version}`,
				);
			}
			if (ctrl) add(rom, "Languages", ctrl.supported_languages.map((l) => languageDisplayName(l)).join(", "));
			add(rom, "Publisher", title?.publisher);
			if (ctrl) {
				add(rom, "Age Ratings", ctrl.age_ratings.map((r) => `${ageRatingDisplayName(r.organization)} ${r.age}+`).join(", "));
			}
			add(rom, "Size", formatBytes(full?.total_content_size ?? info.physical_bytes));
			add(rom, "Base Title", full?.base_application_id_hex);
			add(rom, "Contents", full?.content_count);
			// NCZ crypto sections are stored decrypted; the AES-CTR is re-applied on read.
			add(
				rom,
				"Encryption",
				info.container_kind === "nsz" || info.container_kind === "xcz"
					? "decrypted (ncz sections)"
					: info.tickets.length
						? "encrypted (titlekey)"
						: "encrypted (standard keys)",
			);
			if (full) {
				const req = full.required_system_version;
				if (req > 0) add(rom, "Required System", `${(req >> 26) & 0x3f}.${(req >> 20) & 0x3f}.${(req >> 16) & 0xf}`);
				add(rom, "Storage ID", full.storage_id);
			}
			if (ctrl) {
				add(rom, "Attributes", ctrl.attributes.join(", "));
				add(rom, "Startup Account", ctrl.startup_user_account_name);
				add(rom, "Screenshot", ctrl.screenshot === 0 ? "Allowed" : "Blocked");
				add(rom, "Video Capture", ctrl.video_capture_name);
				add(rom, "Screen Orientation", ctrl.screen_orientation_name);
				add(rom, "Parental Control", ctrl.parental_control_flags.join(", "));
				add(rom, "Add-on Policy", ctrl.addon_install_policy_name);
				add(
					rom,
					"Save Data",
					[
						ctrl.user_account_save && `${formatBytes(ctrl.user_account_save)} user`,
						ctrl.device_save && `${formatBytes(ctrl.device_save)} device`,
						ctrl.bcat_save && `${formatBytes(ctrl.bcat_save)} bcat`,
					]
						.filter(Boolean)
						.join(" · "),
				);
			}
			if (full?.related_titles.length) {
				add(
					rom,
					"Bundled",
					full.related_titles.map((r) => `${r.title_id_hex} (${contentTypeDisplayName(r.kind)} v${r.version})`).join(", "),
				);
			}
			if (!full) add(rom, "Keys", "Provide prod.keys to read title, icon, and content metadata");
			innerTitle = "NCA Files";
			innerFiles = info.files.map((f) => ({
				name: f.name,
				detail: f.partition ? `${formatBytes(f.size)} · ${f.partition}` : formatBytes(f.size),
			}));
			break;
		}
		case "xbox": {
			add(container, "Container", "XISO");
			add(container, "Partition", formatXboxPartitionKind(info.partition_kind));
			add(container, "Logical Size", formatBytes(info.image_size));
			add(container, "Files", `${info.file_count} (${info.dir_count} dirs)`);
			add(container, "File Data", formatBytes(info.total_file_bytes));
			add(container, "Root", `sector ${info.root_sector} · ${formatBytes(info.root_size)}`);
			add(container, "Base", `0x${info.base.toString(16).toUpperCase()}`);
			const xbe = info.xbe;
			const xex = info.xex;
			add(rom, "Title", xbe?.title_name || xex?.title_name);
			add(rom, "Title ID", xbe ? `${xbe.title_id_hex} (${xbe.title_id_code})` : xex?.title_id_hex);
			add(rom, "Content Type", "Game");
			add(rom, "Version", xbe?.version ?? xex?.version);
			add(rom, "Region", (xbe ?? xex)?.region_names.join(", "));
			add(rom, "Size", formatBytes(info.image_size));
			add(rom, "Media ID", xex && xex.media_id.toString(16).padStart(8, "0").toUpperCase());
			add(rom, "Disc", xbe ? xbe.disc_number : xex && `${xex.disc_number}/${xex.disc_count}`);
			add(rom, "Allowed Media", xbe?.allowed_media_names.join(", "));
			add(rom, "Original PE Name", xex?.original_pe_name);
			if (xbe) {
				add(rom, "Ratings", `0x${hex(xbe.ratings, 8)}`);
				if (xbe.cert_timestamp > 0) {
					add(rom, "Cert Timestamp", new Date(xbe.cert_timestamp * 1000).toISOString().slice(0, 10));
				}
				add(rom, "Alternate Title IDs", xbe.alternate_title_ids.map((id) => hex(id, 8)).join(", "));
			}
			if (xex && xex.platform > 0) add(rom, "Platform", xex.platform);
			add(rom, "Base Version", xex?.base_version);
			innerTitle = "Disc Files";
			innerFiles = info.root_entries.map((e) => ({ name: e.name, detail: e.is_dir ? "dir" : formatBytes(e.size) }));
			if (info.file_count + info.dir_count > info.root_entries.length) {
				innerFiles.push({ name: `${info.file_count} files, ${info.dir_count} dirs`, detail: "" });
			}
			break;
		}
		case "xenon": {
			add(container, "Container", "ZArchive");
			add(container, "Compressed Size", formatBytes(info.compressed_size));
			add(container, "Logical Size", formatBytes(info.logical_size));
			add(container, "Ratio", `${xenonRatio(info.logical_size, info.compressed_size).toFixed(1)}%`);
			add(container, "Blocks", info.block_count);
			add(container, "Files", `${info.file_count} (${info.dir_count} dirs)`);
			const xex = info.xex;
			add(rom, "Title", xex?.title_name);
			add(rom, "Title ID", xex?.title_id_hex);
			add(rom, "Content Type", "Game");
			add(rom, "Version", xex?.version);
			add(rom, "Region", xex?.region_names.join(", "));
			add(rom, "Size", formatBytes(info.logical_size));
			add(rom, "Media ID", xex && xex.media_id.toString(16).padStart(8, "0").toUpperCase());
			add(rom, "Disc", xex && `${xex.disc_number}/${xex.disc_count}`);
			add(rom, "Original PE Name", xex?.original_pe_name);
			add(rom, "default.xex", info.has_default_xex ? "present" : "missing");
			if (xex) {
				if (xex.platform > 0) add(rom, "Platform", xex.platform);
				add(rom, "Base Version", xex.base_version);
				add(rom, "Version Raw", `0x${hex(xex.version_raw, 8)}`);
				add(rom, "Allowed Media", `0x${hex(xex.allowed_media, 8)}`);
				add(rom, "Region Raw", `0x${hex(xex.region, 8)}`);
			}
			innerTitle = "Archive Files";
			innerFiles = info.root_entries.map((e) => ({ name: e.name, detail: e.is_file ? formatBytes(e.size) : "dir" }));
			if (info.file_count + info.dir_count > info.root_entries.length) {
				innerFiles.push({ name: `${info.file_count} files, ${info.dir_count} dirs`, detail: "" });
			}
			break;
		}
		case "ps3": {
			add(rom, "Title", info.title);
			add(rom, "Title ID", info.title_id);
			add(rom, "Content Type", "Game");
			add(rom, "Version", info.version);
			add(rom, "Region", info.region);
			add(rom, "Size", formatBytes(info.size_bytes));
			add(rom, "App Version", info.app_ver);
			add(rom, "Resolution", info.resolution);
			add(rom, "Sound Format", info.sound_format);
			add(rom, "Firmware", info.firmware);
			add(rom, "Parental Level", info.parental_level);
			add(rom, "Regions", info.region_count);
			add(rom, "Total Sectors", info.total_sectors);
			if (info.encrypted !== null) add(rom, "Encryption", info.encrypted ? "encrypted" : "decrypted");
			add(rom, "Encrypted Sectors", info.encrypted_sectors);
			innerTitle = "Disc Files";
			innerFiles = info.root_files.map((e) => ({ name: e.name, detail: e.is_dir ? "dir" : formatBytes(e.size) }));
			break;
		}
		case "psx":
		case "psp": {
			rom = discContentRom(info);
			break;
		}
		case "laser_disc": {
			add(container, "Format", `LaserDisc AVI (${info.video_fourcc})`);
			add(container, "Resolution", `${info.video_width}x${info.video_height}`);
			add(container, "FPS", info.fps.toFixed(3));
			add(container, "Duration", `${info.duration_seconds.toFixed(1)}s`);
			add(container, "Frame Count", info.frame_count);
			add(container, "Audio", `${info.audio_channels} ch · ${info.audio_rate} Hz · ${info.audio_bits}-bit`);
			add(container, "Size", formatBytes(info.file_size_bytes));
			add(container, "Interlaced", info.interlaced ? "yes" : "no");
			add(container, "Hunk Bytes", formatBytes(info.bytes_per_frame));
			add(container, "Fields", info.fields);
			if (info.vbi) {
				const vbi = info.vbi;
				add(container, "Disc Type", ldDiscTypeLabel(vbi.disc_type));
				if (vbi.cav_picture_min != null && vbi.cav_picture_max != null) {
					add(container, "Picture Range", `${vbi.cav_picture_min}-${vbi.cav_picture_max}`);
				}
				if (vbi.clv_start && vbi.clv_end) {
					add(container, "Time Range", `${ldClvTime(vbi.clv_start)}-${ldClvTime(vbi.clv_end)}`);
				}
				if (vbi.chapter_min != null && vbi.chapter_max != null) {
					add(container, "Chapters", `${vbi.chapter_min}-${vbi.chapter_max}`);
				}
				add(container, "White Flags", vbi.white_flag_count);
				add(container, "Lead-in / Lead-out", `${vbi.lead_in ? "yes" : "no"} / ${vbi.lead_out ? "yes" : "no"}`);
				add(container, "Fields Without Code", vbi.fields_without_code);
			}
			break;
		}
		case "nds": {
			add(rom, "Title", info.game_title);
			add(rom, "Title ID", info.game_code);
			add(rom, "Content Type", "Game");
			add(rom, "Version", `v${info.rom_version}`);
			add(rom, "Publisher", info.maker_code);
			add(rom, "Unit Code", info.unit_code_name);
			add(rom, "Size", formatBytes(info.physical_bytes));
			add(rom, "Capacity", formatBytes(info.capacity_bytes));
			add(rom, "Encryption", info.secure_area === "not_present" ? "not present" : info.secure_area);
			crcField(rom, "Header CRC16", info.header_crc16, info.header_crc16_computed, info.header_crc16_valid, 4);
			if (info.banner) {
				const bannerTitle = englishFirst(info.banner.titles.entries, (e) => e[0]);
				add(rom, "Banner Title", bannerTitle?.[1]);
				add(rom, "Banner Languages", info.banner.titles.entries.map((e) => languageDisplayName(e[0])).join(", "));
				crcField(
					rom,
					"Banner CRC16",
					info.banner.banner_crc16,
					info.banner.banner_crc16_computed,
					info.banner.banner_crc16_valid,
					4,
				);
			}
			innerTitle = "ARM Binaries";
			innerFiles = [
				{ name: "ARM9", detail: `${formatBytes(info.arm9.size)} · entry 0x${hex(info.arm9.entry_address, 8)}` },
				{ name: "ARM7", detail: `${formatBytes(info.arm7.size)} · entry 0x${hex(info.arm7.entry_address, 8)}` },
			];
			break;
		}
	}

	return {
		container,
		rom,
		innerTitle,
		innerFiles,
		hashes,
		contentType: rom.find((f) => f.label === "Content Type")?.value ?? null,
	};
}

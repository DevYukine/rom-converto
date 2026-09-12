import type { InvokeArgs } from "@tauri-apps/api/core";
import type { Event, EventCallback, UnlistenFn } from "@tauri-apps/api/event";
import type { OpenDialogOptions, SaveDialogOptions } from "@tauri-apps/plugin-dialog";
import type { RunOutcome } from "~/types";

// Dev-browser stand-in for the Tauri IPC: a command handler table plus a fake
// event emitter so every UI path is exercisable without a Tauri window. Canned
// responses are shape-correct for each consumer; names are neutral placeholders.

const FAKE_DIR = "~/roms/switch";
const FAKE_LIB = "~/roms/library";
const HEX40 = "3a7bd3e2360a3d29eea436fcfb7e44c735d117c4";
const HEX40B = "9f1c0b7ad5e84462b1c3f0a29d6e5471c8b2a3f0";

type Listener = (event: Event<unknown>) => void;

const listeners = new Map<string, Set<Listener>>();
let nextEventId = 0;

function emit(name: string, payload: unknown) {
	const set = listeners.get(name);
	if (!set) return;
	const event = { event: name, id: nextEventId++, payload } as Event<unknown>;
	set.forEach((cb) => cb(event));
}

const delay = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

// Per-task cancel flags. A task is cancelled by cmd_cancel({ taskId }); the fake
// progress loop throws the same message the real backend uses so callers route
// it to the cancelled state instead of the failed state. Progress and row
// events go out on the separate progress key, exactly as the backend does.
const cancelled = new Set<string>();

async function fakeProgress(taskId: string, progressKey: string, ms = 1500, total = 100) {
	cancelled.delete(taskId);
	emit("progress", { task_id: progressKey, kind: "start", total, current: 0, message: "" });
	const steps = 10;
	for (let i = 1; i <= steps; i++) {
		await delay(ms / steps);
		if (cancelled.has(taskId)) {
			cancelled.delete(taskId);
			throw "operation cancelled";
		}
		emit("progress", { task_id: progressKey, kind: "inc", total, current: (i / steps) * total, message: "" });
	}
	emit("progress", { task_id: progressKey, kind: "finish", total, current: total, message: "" });
}

function baseName(path: string): string {
	const norm = path.replace(/\\/g, "/");
	return norm.slice(norm.lastIndexOf("/") + 1);
}

function extOf(path: string): string {
	const name = baseName(path);
	const dot = name.lastIndexOf(".");
	return dot === -1 ? "" : name.slice(dot + 1).toLowerCase();
}

// A path containing "fail", or setting `__mockFailNext = true` on window,
// rejects the next invoke so the queue's failed state and Retry flow are
// exercisable in the browser.
function maybeFail(a: Record<string, unknown>): void {
	const w = globalThis as Record<string, unknown>;
	if (w.__mockFailNext) {
		w.__mockFailNext = false;
		throw new Error("mock failure: simulated backend error");
	}
	const p = String(a.input ?? a.inputPath ?? a.cuePath ?? "");
	if (p.includes("fail")) throw new Error("mock failure: simulated backend error");
}

function runOutcome(): RunOutcome {
	const inputBytes = 12_400_000_000;
	const outputBytes = 4_600_000_000;
	return {
		message: "Done",
		status: 0,
		records: [],
		input_bytes: inputBytes,
		output_bytes: outputBytes,
		comparison: {
			input_bytes: inputBytes,
			output_bytes: outputBytes,
			ratio_pct: 37.1,
			input_format: "iso",
			output_format: "chd",
			output_sha1: HEX40,
		},
		data: null,
	};
}

// Alternating pass/fail so a batch of staged files shows a mix of verdicts.
const flips = new Map<string, number>();
function nextIsPass(command: string): boolean {
	const n = flips.get(command) ?? 0;
	flips.set(command, n + 1);
	return n % 2 === 0;
}

// --- info samples (mirror types/generated/info.ts InfoResult, neutral placeholder names) ---

const NX_INFO = {
	kind: "nx",
	container_kind: "nsp",
	is_compressed: false,
	distribution: "digital",
	structure: "scene",
	physical_bytes: 12_400_000_000,
	files: [
		{ partition: null, name: "program.nca", abs_offset: 1024, size: 11_800_000_000 },
		{ partition: null, name: "control.nca", abs_offset: 11_800_100_000, size: 2_000_000 },
		{ partition: null, name: "meta.cnmt.nca", abs_offset: 11_802_200_000, size: 16_384 },
	],
	nca_names: ["program.nca", "control.nca", "meta.cnmt.nca"],
	cnmt_nca_names: ["meta.cnmt.nca"],
	tickets: [],
	xci_partitions: null,
	full: {
		application_title_id: 0x0100aaaa00bbb000,
		application_title_id_hex: "0100AAAA00BBB000",
		title_version: 0,
		title_kind: "application",
		storage_id: 0,
		attributes: 0,
		required_system_version: 0,
		required_application_version: null,
		base_application_id: null,
		base_application_id_hex: null,
		content_count: 3,
		total_content_size: 12_400_000_000,
		contents: [{ content_id: HEX40.slice(0, 32), content_type: "program", size: 11_800_000_000 }],
		related_titles: [],
		control: {
			titles: [{ language: "AmericanEnglish", name: "Sample Switch Title", publisher: "Sample Publisher" }],
			display_version: "1.0.0",
			startup_user_account: 0,
			startup_user_account_name: "None",
			screenshot: 0,
			video_capture: 0,
			video_capture_name: "Disabled",
			attribute_flag: 0,
			attributes: [],
			supported_language_bitmask: 1,
			supported_languages: ["AmericanEnglish"],
			parental_control_flag: 0,
			parental_control_flags: [],
			user_account_save: 0,
			user_account_save_journal: 0,
			device_save: 0,
			device_save_journal: 0,
			bcat_save: 0,
			rating_age: [],
			age_ratings: [],
			addon_install_policy: 0,
			addon_install_policy_name: "",
			screen_orientation: 0,
			screen_orientation_name: "Both",
			icon: null,
			icon_language: null,
		},
	},
};

const CTR_INFO = {
	kind: "ctr",
	format: "cia",
	physical_bytes: 512_000_000,
	title_id: "0004000000123400",
	program_id: "0004000000123400",
	product_code: "CTR-P-SMPL",
	maker_code: "01",
	maker_name: "Sample",
	cartridge_size: null,
	ncch_encrypted: false,
	seed_crypto: false,
	seed_found: null,
	seed_keyy: null,
	content_kind: "game",
	smdh: {
		titles: [
			// Japanese comes first in SMDH layout order; the card must still pick English.
			{ language: "Japanese", short_description: "サンプルタイトル", long_description: "サンプルタイトル", publisher: "サンプル" },
			{ language: "English", short_description: "Sample 3DS Title", long_description: "Sample 3DS Title", publisher: "Sample Publisher" },
		],
		region_lock: 0,
		region_names: ["USA", "Europe"],
		flags: 0,
		eula_version_major: 0,
		eula_version_minor: 0,
		age_ratings: [],
	},
	icon: null,
	small_icon: null,
	compressed: false,
	ncsd_partitions: [],
	cia_contents: [
		{ index: 0, content_id: "00000000", size: 500_000_000, encrypted: false },
		{ index: 1, content_id: "00000001", size: 12_000_000, encrypted: false },
	],
};

const DOL_INFO = {
	kind: "dol",
	physical_bytes: 1_459_978_240,
	container: "iso",
	game_id: "GSMPLE",
	maker_code: "01",
	maker_name: "Sample",
	disc_number: 0,
	disc_version: 0,
	audio_streaming: false,
	game_name: "Sample GameCube Title",
	region: "NTSC-U",
	apploader_date: null,
	banner: {
		format: "Bnr2",
		titles: [
			// Non-English first; the card must still pick English.
			{ language: "German", short_game_name: "Beispieltitel", short_maker: "Beispiel", long_game_name: "Beispiel GameCube Titel", long_maker: "Beispiel", description: "" },
			{ language: "English", short_game_name: "Sample GC Title", short_maker: "Sample", long_game_name: "Sample GameCube Banner Title", long_maker: "Sample Publisher", description: "" },
		],
	},
	banner_image: null,
	fst_root: [
		{ name: "opening.bnr", size: 98_304, is_dir: false },
		{ name: "audio", size: 0, is_dir: true },
		{ name: "scenes", size: 0, is_dir: true },
	],
	fst_file_count: 812,
	fst_dir_count: 46,
};

const RVL_INFO = {
	kind: "rvl",
	physical_bytes: 4_699_979_776,
	container: "iso",
	game_id: "RSMPLE",
	maker_code: "01",
	maker_name: "Sample",
	disc_number: 0,
	disc_version: 0,
	game_name: "Sample Wii Title",
	region: "NTSC-U",
	partitions: [
		{ offset: 0x50000, partition_type: 1, group: 0, kind: "UPDATE" },
		{ offset: 0xf800000, partition_type: 0, group: 0, kind: "DATA" },
	],
	tmd: {
		title_id: 0x0000000152534d50,
		title_id_hex: "0000000152534D50",
		title_version: 0,
		system_version: 0,
		ios_slot: null,
		region_name: "NTSC-U",
		content_count: 1,
		access_rights: 0,
	},
	imet_names: {
		entries: [
			["japanese", "サンプルタイトル"],
			["english", "Sample Wii IMET Title"],
		],
	},
	image: null,
};

const WUP_INFO = {
	kind: "wup",
	title_id: 0x0005000010112200,
	title_id_hex: "0005000010112200",
	title_type: "application",
	content_kind: "game",
	title_version: 0,
	group_id: 0,
	access_rights: 0,
	content_count: 5,
	total_content_size: 22_800_000_000,
	os_version: null,
	sdk_version: null,
	source_kind: "nus",
	bundled_titles: [
		{ title_id: 0x0005000010112200, title_id_hex: "0005000010112200", title_type: "Game", title_version: 0 },
		{ title_id: 0x000500001e112200, title_id_hex: "000500001E112200", title_type: "Update", title_version: 16 },
		{ title_id: 0x0005000c10112200, title_id_hex: "0005000C10112200", title_type: "DLC", title_version: 0 },
	],
	update_version: 16,
	image: null,
	meta: {
		long_names: { entries: [["english", "Sample Wii U Title"]] },
		short_names: { entries: [["english", "Sample Title"]] },
		publishers: { entries: [["english", "Sample Publisher"]] },
		product_code: "WUP-P-SMPL",
		company_code: null,
		company_name: null,
		region: null,
		region_names: ["USA"],
		title_id: null,
		os_version: null,
		app_size: null,
		group_id: null,
		boss_id: null,
		mastering_date: null,
		content_platform: null,
		logo_type: null,
		app_launch_type: null,
		invisible_flag: null,
		no_managed_flag: null,
		eula_version: null,
		drc_use: null,
		e_manual: null,
		e_manual_version: null,
		ext_dev_nunchaku: null,
		ext_dev_classic: null,
		ext_dev_urcc: null,
		ext_dev_board: null,
		ext_dev_usb_keyboard: null,
		ext_dev_etc: null,
		ext_dev_etc_name: null,
		save_size: null,
		common_save_size: null,
		account_save_size: null,
		boss_size: null,
		common_boss_size: null,
		account_boss_size: null,
		network_use: null,
		online_account_use: null,
		age_ratings: {},
	},
	disc_partitions: [],
};

const CHD_INFO = {
	kind: "chd",
	version: 5,
	compressors: ["cdlz", "cdzl", "cdfl"],
	hunk_bytes: 19_584,
	unit_bytes: 2_448,
	hunk_count: 50_000,
	logical_bytes: 734_003_200,
	physical_bytes: 280_000_000,
	compression_ratio: 38.1,
	raw_sha1: HEX40,
	sha1: HEX40B,
	parent_sha1: null,
	tracks: [{ number: 1, track_type: "MODE1_RAW", frames: 330_000, pregap: 0, subtype: null, pgtype: null, pgsub: null, postgap: null }],
	metadata_tags: [],
	version_string: "MAME compress 0.264",
	dvd: null,
};

const CSO_INFO = {
	kind: "cso",
	format: "cso",
	version: 1,
	block_size: 2_048,
	index_shift: 0,
	uncompressed_size: 1_500_000_000,
	physical_bytes: 900_000_000,
	compression_ratio: 40.0,
	block_count: 732_000,
	raw_block_count: 20_000,
};

const XBOX_INFO = {
	kind: "xbox",
	partition_kind: "trimmed",
	base: 0x10000,
	root_sector: 0x108,
	root_size: 16_384,
	file_count: 420,
	dir_count: 38,
	total_file_bytes: 6_800_000_000,
	image_size: 6_810_000_000,
	xbe: {
		title_id: 0x4d5a0058,
		title_id_hex: "4D5A0058",
		title_id_code: "MZ-058",
		title_name: "Halo 2",
		alternate_title_ids: [],
		allowed_media: 0xffffffff,
		allowed_media_names: ["DVD", "HardDisk"],
		region: 0x00000001,
		region_names: ["North America"],
		ratings: 0,
		disc_number: 1,
		version: 1,
		cert_timestamp: 0,
	},
	xex: null,
	root_entries: [
		{ name: "default.xbe", size: 4_100_000, is_dir: false },
		{ name: "media", size: 0, is_dir: true },
	],
};

const XENON_INFO = {
	kind: "xenon",
	file_count: 512,
	dir_count: 44,
	logical_size: 7_200_000_000,
	compressed_size: 4_100_000_000,
	block_count: 28_100,
	has_default_xex: true,
	xex: {
		title_id: 0x4d5308e3,
		title_id_hex: "4D5308E3",
		media_id: 0x4a3c1f02,
		version: "1.0.0.0",
		version_raw: 0,
		base_version: "1.0.0.0",
		base_version_raw: 0,
		disc_number: 1,
		disc_count: 1,
		platform: 0,
		original_pe_name: "default.xex",
		region: 0x000000ff,
		region_names: ["North America", "Europe"],
		allowed_media: 0xffffffff,
		title_name: "Gears of War",
		icon: {
			png_bytes: [
				137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 31, 21,
				196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240, 31, 0, 5, 0, 1, 255, 137, 153, 61,
				29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
			],
			width: 64,
			height: 64,
		},
	},
	root_entries: [
		{ name: "default.xex", size: 9_400_000, is_file: true },
		{ name: "data", size: 0, is_file: false },
	],
};

const NTR_INFO = {
	kind: "ntr",
	physical_bytes: 33_554_432,
	game_title: "SAMPLE GAME",
	game_code: "ASME",
	maker_code: "01",
	unit_code: 0,
	unit_code_name: "NDS",
	region: 0,
	rom_version: 0,
	device_capacity: 9,
	capacity_bytes: 33_554_432,
	ntr_rom_size: 33_554_432,
	arm9: { rom_offset: 0x4000, entry_address: 0x2000000, load_address: 0x2000000, size: 0x40000 },
	arm7: { rom_offset: 0x8000, entry_address: 0x2380000, load_address: 0x2380000, size: 0x30000 },
	fnt_offset: 0x50000,
	fnt_size: 4_096,
	fat_offset: 0x54000,
	fat_size: 2_048,
	header_crc16: 0xabcd,
	header_crc16_computed: 0xabcd,
	header_crc16_valid: true,
	secure_area: "decrypted",
	banner: {
		banner_version: 1,
		titles: { entries: [["english", "Sample NDS Title"]] },
		banner_crc16: 0x1234,
		banner_crc16_computed: 0x1234,
		banner_crc16_valid: true,
		icon: null,
	},
};

const RETRO_INFO = {
	kind: "retro",
	file_size: 1_048_576,
	details: {
		system: "nes",
		nes2: false,
		prg_rom_bytes: 131_072,
		chr_rom_bytes: 8_192,
		mapper: 1,
		submapper: null,
		mirroring: "Horizontal",
		battery: false,
		trainer: false,
		four_screen: false,
		console_type: "NES",
		timing: "NTSC",
		prg_ram_bytes: null,
		prg_nvram_bytes: null,
		chr_ram_bytes: null,
		chr_nvram_bytes: null,
	},
};

const PBP_INFO = {
	kind: "pbp",
	physical_bytes: 180_000_000,
	version: 0x00000100,
	title: "Sample PSP Title",
	disc_id: "ULUS10000",
	disc_version: "1.00",
	category: "MG",
	category_label: "Game",
	content_kind: "game",
	psp_system_ver: "6.61",
	parental_level: 1,
	region: null,
	icon: null,
	segments: [
		{ name: "PARAM.SFO", offset: 0, size: 1_024, present: true },
		{ name: "ICON0.PNG", offset: 1_024, size: 20_000, present: true },
		{ name: "DATA.PSAR", offset: 21_024, size: 179_900_000, present: true },
	],
	psar_kind: { kind: "npumdimg" },
};

const VPK_INFO = {
	kind: "vpk",
	title: "Sample Vita Title",
	title_id: "PCSE00001",
	content_id: "EP0001-PCSE00001_00-SAMPLEVITA000001",
	app_ver: "01.00",
	category: "gd",
	category_label: "Game",
	content_kind: "game",
	icon: null,
	file_count: 420,
	total_size: 900_000_000,
};

const PKG_INFO = {
	kind: "pkg",
	content_id: "EP0001-PCSE00001_00-SAMPLEVITA000001",
	pkg_revision: 1,
	pkg_type: 1,
	platform: "vita",
	content_type: 21,
	content_type_label: "Game",
	content_kind: "game",
	category: "gd",
	title: "Sample Vita Title",
	title_id: "PCSE00001",
	icon: null,
	item_count: 420,
	total_size: 900_000_000,
	data_offset: 0x1000,
	data_size: 899_990_000,
	key_type: 2,
	drm_type: 1,
	package_flags: 0,
	meta_ids: [1, 2, 4, 5, 6, 7, 8, 10, 11, 12, 13, 14, 15, 16, 17, 18],
};

const PS4_PKG_INFO = {
	kind: "ps4_pkg",
	content_id: "UP9000-CUSA00001_00-SAMPLEPS4GAME000",
	finalized: true,
	drm_type: 0xf,
	content_type: 0x1a,
	content_type_label: "PS4 game data",
	content_flags: 0,
	content_flag_labels: [],
	content_kind: "game",
	version_date: 0x20240115,
	title: "Sample PS4 Title",
	title_id: "CUSA00001",
	category: "gd",
	category_label: "Game",
	app_ver: "01.00",
	version: "01.00",
	system_ver: "09.00",
	app_type: 1,
	app_type_label: "Paid standalone full",
	parental_level: 5,
	ps2_classic: false,
	emu_version: null,
	icon: null,
	background: null,
	entry_count: 2,
	entries: [
		{ id: 0x1000, name: "param.sfo", offset: 0x2000, size: 4_096, encrypted: false, key_index: 0 },
		{ id: 0x1200, name: "icon0.png", offset: 0x3000, size: 20_000, encrypted: false, key_index: 0 },
	],
	pfs_image_offset: 0x100000,
	pfs_image_size: 6_800_000_000,
	package_size: 6_800_100_000,
	file_size: 6_800_100_000,
};

const PS5_PKG_INFO = {
	kind: "ps5_pkg",
	content_id: "UP9000-PPSA00001_00-SAMPLEPS5GAME000",
	image: "cnt",
	signed: true,
	finalized: true,
	drm_type: 0xf,
	content_type: 0x20,
	content_type_label: "PS5 game data",
	content_flags: 0,
	content_flag_labels: [],
	content_kind: "game",
	version_date: 0x20240115,
	title: "Sample PS5 Title",
	title_id: "PPSA00001",
	default_language: "en-US",
	content_version: "01.000.000",
	target_content_version: null,
	master_version: "01.000.000",
	required_system_version: "05.00",
	sdk_version: "07.00",
	application_category_type: 0,
	application_category_label: "Native game",
	application_drm_type: "standard",
	creation_date: "2024-01-15",
	icon: null,
	background: null,
	entry_count: 2,
	entries: [
		{ id: 0x2000, name: "param.json", offset: 0x2000, size: 8_192, encrypted: false, key_index: 0 },
		{ id: 0x1200, name: "icon0.png", offset: 0x4000, size: 24_000, encrypted: false, key_index: 0 },
	],
	pfs_image_offset: 0x100000,
	pfs_image_size: 9_200_000_000,
	package_size: 9_200_100_000,
	file_size: 9_200_100_000,
};

const INFO_SAMPLES: Record<string, unknown> = {
	nx: NX_INFO,
	ctr: CTR_INFO,
	dol: DOL_INFO,
	rvl: RVL_INFO,
	wup: WUP_INFO,
	chd: CHD_INFO,
	cso: CSO_INFO,
	xbox: XBOX_INFO,
	xenon: XENON_INFO,
	ntr: NTR_INFO,
	retro: RETRO_INFO,
	pbp: PBP_INFO,
	vpk: VPK_INFO,
	pkg: PKG_INFO,
	ps4_pkg: PS4_PKG_INFO,
	ps5_pkg: PS5_PKG_INFO,
};

function infoKindFor(path: string): string {
	const e = extOf(path);
	if (["nsp", "xci", "nsz", "xcz"].includes(e)) return "nx";
	if (["cia", "3ds", "cci", "cxi", "ncch", "3dsx", "zcia", "zcci", "zcxi", "z3dsx"].includes(e)) return "ctr";
	if (["gcm", "gcz"].includes(e)) return "dol";
	if (["wbfs", "wia"].includes(e)) return "rvl";
	if (["wud", "wux"].includes(e)) return "wup";
	if (["chd"].includes(e)) return "chd";
	if (["cso", "zso", "dax"].includes(e)) return "cso";
	if (["xiso"].includes(e)) return "xbox";
	if (["zar"].includes(e)) return "xenon";
	if (["nds", "dsi"].includes(e)) return "ntr";
	if (["pbp"].includes(e)) return "pbp";
	if (["vpk"].includes(e)) return "vpk";
	if (["pkg"].includes(e)) return "pkg";
	if (
		[
			"nes", "sfc", "smc", "z64", "n64", "v64", "gb", "gbc", "gba", "md", "gen", "smd", "32x", "sms", "gg", "vb", "ws",
			"wsc", "ngp", "ngc", "lnx", "a78", "fds", "gdi",
		].includes(e)
	)
		return "retro";
	return "nx";
}

// --- verify samples (one variant per call, mixed pass/fail), keyed by console ---

function verifyResult(console: string): unknown {
	const pass = nextIsPass(console);
	switch (console) {
		case "ctr":
			return pass
				? { format: "Cia", legitimacy: "Legitimate", content_hashes_valid: true, title_id: "0004000000123400", details: ["Signature: valid", "Ticket: present"] }
				: { format: "Cia", legitimacy: "Illegitimate", content_hashes_valid: false, title_id: "0004000000123400", details: ["Signature: INVALID", "Content hash mismatch at index 2"] };
		case "dol":
			return pass
				? { ok: true, rvz_structure: { ok: true }, disc_sha1: HEX40, structural: { notes: [] } }
				: { ok: false, rvz_structure: { ok: false }, disc_sha1: HEX40, structural: { notes: ["Block 42 checksum mismatch"] } };
		case "rvl":
			return pass
				? { ok: true, rvz_structure: { ok: true }, partitions: [{ ok: true, mismatched_clusters: 0, offset: 0, note: null }] }
				: { ok: false, rvz_structure: { ok: true }, partitions: [{ ok: false, mismatched_clusters: 12, offset: 0x100000, note: null }] };
		case "wup":
			return pass
				? { ok: true, kind: "NUS", titles: [{ title_id_hex: "0005000010112200", ok: true, verified_content: 5, mismatched_content: 0, skipped_content: 0 }] }
				: { ok: false, kind: "NUS", titles: [{ title_id_hex: "0005000010112200", ok: false, verified_content: 3, mismatched_content: 2, skipped_content: 0 }] };
		case "nx":
			return pass
				? { ok: true, kind: "NSP", ncas: [{ ok: true, name: "program.nca", partition: null, mismatched_sections: 0 }] }
				: { ok: false, kind: "NSP", ncas: [{ ok: false, name: "program.nca", partition: null, mismatched_sections: 1 }] };
		case "cso":
			return pass ? { ok: true, mismatches: 0 } : { ok: false, mismatches: 3 };
		case "xenon":
			return { blocks: 28_100, logical_bytes: 7_200_000_000, hash_ok: pass };
		default:
			return { ok: pass };
	}
}

// --- DAT samples ---

type ScanRow = {
	path: string;
	status: string;
	game_name: string | null;
	game_id: string | null;
	match_algo: string | null;
	canonical_stem: string | null;
	error: string | null;
};

const BLANK_ROW = { game_name: null, game_id: null, match_algo: null, canonical_stem: null, error: null };

// A folder whose path mentions "cdn" mimics a title-server layout: hundreds of
// small identically named content files under per-title directories.
function scanFixture(dir: string): ScanRow[] {
	if (!/cdn/i.test(dir)) {
		return [
			{ ...BLANK_ROW, path: `${dir}/title-a.chd`, status: "matched", game_name: "Sample Title A", game_id: "g-1", match_algo: "crc32" },
			{ ...BLANK_ROW, path: `${dir}/title-b.chd`, status: "misnamed", game_name: "Sample Title B", game_id: "g-2", match_algo: "crc32", canonical_stem: "Sample Title B (USA)" },
			{ ...BLANK_ROW, path: `${dir}/title-c.iso`, status: "hint", game_name: "Sample Title C", game_id: "g-3" },
			{ ...BLANK_ROW, path: `${dir}/title-d.bin`, status: "unknown" },
			{ ...BLANK_ROW, path: `${dir}/notes.txt`, status: "unsupported" },
			{ ...BLANK_ROW, path: `${dir}/title-e.chd`, status: "failed", error: "Hash read error" },
		];
	}
	const rows: ScanRow[] = [];
	for (let t = 0; t < 40; t++) {
		const title = `${dir}/000400000${(0x1000 + t * 0x100).toString(16)}`;
		rows.push(
			{ ...BLANK_ROW, path: `${title}/tmd`, status: "unsupported" },
			{ ...BLANK_ROW, path: `${title}/cetk`, status: "unsupported" },
		);
		for (let c = 0; c < 8; c++) {
			const path = `${title}/${c.toString(16).padStart(8, "0")}`;
			if (c === 0)
				rows.push({ ...BLANK_ROW, path, status: "matched", game_name: `Sample Title ${t + 1}`, game_id: `g-${t + 1}`, match_algo: "crc32" });
			else if (c === 7 && t % 5 === 0) rows.push({ ...BLANK_ROW, path, status: "failed", error: "Hash read error" });
			else rows.push({ ...BLANK_ROW, path, status: "unknown" });
		}
	}
	return rows;
}

// Mirrors the real scan's progress protocol: an outer file counter on the
// progress key, per-file bytes on `<progressKey>-file`, and zero-total starts
// for the indeterminate network phases.
async function datScan(taskId: string, progressKey: string, dir: string): Promise<unknown> {
	const rows = scanFixture(dir);
	const many = rows.length > 20;
	const progress = (task_id: string, kind: string, total: number, current: number, message = "") =>
		emit("progress", { task_id, kind, total, current, message });
	const checkCancel = () => {
		if (cancelled.has(taskId)) {
			cancelled.delete(taskId);
			throw "operation cancelled";
		}
	};
	const row = (r: ScanRow) => emit(`${progressKey}-row`, { kind: "dat_scan", ...r });
	cancelled.delete(taskId);
	progress(progressKey, "phase", 0, 0, "Collecting files");
	await delay(400);
	progress(progressKey, "start", rows.length, 0, "Hashing files");
	for (let i = 0; i < rows.length; i++) {
		const r = rows[i]!;
		const size = many ? 16_384 : 700_000_000;
		const steps = many ? 1 : 4;
		progress(`${progressKey}-file`, "start", size, 0, `Hashing ${baseName(r.path)}`);
		for (let s = 1; s <= steps; s++) {
			await delay(many ? 12 : 60);
			checkCancel();
			progress(`${progressKey}-file`, "inc", 0, (size / steps) * s);
		}
		progress(`${progressKey}-file`, "finish", 0, 0);
		const settled = r.status === "unsupported" || r.status === "failed";
		row(settled ? r : { ...r, ...BLANK_ROW, status: "pending" });
		progress(progressKey, "inc", 0, i + 1);
	}
	progress(progressKey, "start", 0, 0, `Matching ${rows.length} files`);
	await delay(many ? 1500 : 600);
	checkCancel();
	for (const r of rows) row(r);
	progress(progressKey, "finish", 0, 0);
	const tally = (status: string) => rows.filter((r) => r.status === status).length;
	return {
		matched: tally("matched"),
		misnamed: tally("misnamed"),
		hint: tally("hint"),
		unknown: tally("unknown"),
		unsupported: tally("unsupported"),
		failed: tally("failed"),
		rows,
	};
}

// Streams one row per unit on `<progressKey>-row`, driving the progress bar
// alongside it, the way the runner streams a directory job.
async function streamRows<T>(
	taskId: string,
	progressKey: string,
	rows: T[],
	toRow: (row: T) => unknown,
): Promise<void> {
	cancelled.delete(taskId);
	emit("progress", { task_id: progressKey, kind: "start", total: rows.length, current: 0, message: "" });
	for (let i = 0; i < rows.length; i++) {
		await delay(220);
		if (cancelled.has(taskId)) {
			cancelled.delete(taskId);
			throw "operation cancelled";
		}
		emit(`${progressKey}-row`, toRow(rows[i]!));
		emit("progress", { task_id: progressKey, kind: "inc", total: rows.length, current: i + 1, message: "" });
	}
	emit("progress", { task_id: progressKey, kind: "finish", total: rows.length, current: rows.length, message: "" });
}

function datMatch(path: string): Record<string, unknown> {
	const pass = nextIsPass("dat.verify");
	return {
		kind: "verify",
		path,
		verdict: pass ? "verified" : "failed",
		match_algo: "sha1",
		game_name: "Sample Title",
		platform: "Sample Platform",
		signature_group: null,
		dat_file: "Sample - Games.dat",
		dat_file_id: "1",
		dat_version: "2024-01",
		external_ids: [],
		tracks: [],
		size_bytes: 734_003_200,
		match: null,
		error: pass ? null : "Full hash does not match the database entry.",
	};
}

// A directory input settles as one DatVerifyData, streaming a row per unit as
// it goes; a file input as the single DatMatchData, exactly as the runner
// returns them.
async function datVerify(taskId: string, progressKey: string, input: string): Promise<unknown> {
	if (extOf(input)) {
		await fakeProgress(taskId, progressKey, 900);
		return datMatch(input);
	}
	const rows = [datMatch(`${input}/title-a.chd`), datMatch(`${input}/title-b.chd`)];
	await streamRows(taskId, progressKey, rows, (row) => ({ kind: "dat_match", ...row }));
	const failed = rows.filter((r) => r.verdict === "failed").length;
	return { verified: rows.length - failed, hint: 0, unknown: 0, unsupported: 0, failed, rows };
}

function datRename(dir: string, dry: boolean): unknown {
	const rows = [
		{ from: `${dir}/title-a.chd`, to: `${dir}/Sample Title A (USA).chd`, action: dry ? "would_rename" : "renamed", detail: null },
		{ from: `${dir}/title-b.chd`, to: `${dir}/Sample Title B (USA).chd`, action: dry ? "would_rename" : "renamed", detail: null },
		{ from: `${dir}/Sample Title C (USA).chd`, to: null, action: "already_canonical", detail: null },
		{ from: `${dir}/unmatched.bin`, to: null, action: "skip_unmatched", detail: "No DAT match" },
	];
	return { rows, dry_run: dry, renamed: 2, skipped: 2, failed: 0 };
}

// --- organize samples ---

// One organized library: conversions, a zip, a copy, one skip and one failure
// so every status chip has something to show. Dry-run rows are `planned` and
// carry the planner's decision as their detail, as the runner does.
async function organizeRun(taskId: string, progressKey: string, input: string, dry: boolean): Promise<unknown> {
	const out = "~/roms/organized";
	const unit = (
		input: string,
		output: string | null,
		label: string | null,
		action: string,
		status: string,
		detail: string | null,
	) => ({
		input,
		output,
		console: label,
		action,
		status,
		detail: dry && status === "ok" ? "New" : detail,
		planned: dry,
		input_bytes: 734_003_200,
		output_bytes: status === "ok" ? 366_000_000 : 0,
		elapsed_ms: 1_250,
	});
	const rows = [
		unit(`${input}/Zelda Four Swords.gcm`, `${out}/GameCube/Zelda Four Swords.rvz`, "GameCube", "dol.compress", "ok", null),
		unit(`${input}/Panzer Dragoon.cue`, `${out}/Saturn/Panzer Dragoon.chd`, "Saturn", "chd.compress", "ok", null),
		unit(`${input}/Pokemon Emerald.gba`, `${out}/Game Boy Advance/Pokemon Emerald.zip`, "Game Boy Advance", "zip", "ok", null),
		unit(`${input}/Sample Game.rvz`, `${out}/GameCube/Sample Game.rvz`, "GameCube", "copy", "ok", null),
		unit(`${input}/readme.txt`, null, null, "skip", "skipped", "unrecognized"),
		unit(`${input}/Already In Place.chd`, `${out}/PS1/Already In Place.chd`, "PS1", "copy", "skipped", "already in place"),
		unit(`${input}/Broken Disc.iso`, null, "PS2", "chd.compress", "failed", "Disc read error"),
	];
	await streamRows(taskId, progressKey, rows, (row) => ({ kind: "organize", ...row }));
	return {
		rows,
		dry_run: dry,
		ok: rows.filter((r) => r.status === "ok").length,
		skipped: rows.filter((r) => r.status === "skipped").length,
		failed: rows.filter((r) => r.status === "failed").length,
		playlists: dry
			? []
			: [
					{
						base_title: "Panzer Dragoon",
						output: `${out}/Saturn/Panzer Dragoon.m3u`,
						contents: "Panzer Dragoon.chd",
						disc_count: 1,
						has_duplicate_numbers: false,
					},
				],
	};
}

// One file's digests, holding only the algorithms the request asked for.
function hashDigests(algo: string): Record<string, unknown> {
	const values: Record<string, string> = {
		crc32: "1a2b3c4d",
		md5: "d41d8cd98f00b204e9800998ecf8427e",
		sha1: HEX40,
		sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
	};
	const asked = (algo || "crc32,sha1").split(",");
	const digests: Record<string, unknown> = { size_bytes: 12_400_000_000 };
	for (const name of asked) if (values[name]) digests[name] = values[name];
	return digests;
}

// --- handler table ---

type Handler = (args: Record<string, unknown>) => Promise<unknown>;

export const handlers: Record<string, Handler> = {
	app_display_version: async () => "1.4.0",
	cmd_nx_keys_resolve: async (a) => (a.keys ? String(a.keys) : "~/.switch/prod.keys"),
	cmd_config_path: async () => "~/.config/rom-converto/rom-converto.toml",
	cmd_load_config: async () => ({
		presets: {
			"archive-max": { chd: { hunk_size: null, on_conflict: "overwrite", output_dir: "~/roms/chd", report: false } },
			"switch-fast": { nx: { level: 12, mode: "solid", on_conflict: "overwrite" } },
		},
		dat: { input_checksum_min: null, input_checksum_max: null },
	}),
	cmd_save_preset: async () => null,
	cmd_delete_preset: async () => null,
	cmd_save_icon: async () => null,
	cmd_write_report: async () => null,
	cmd_file_size: async () => 12_400_000_000,
	cmd_scan_dir: async (a) => {
		const dir = typeof a.dir === "string" ? a.dir : FAKE_DIR;
		// A path with an extension is a file, not a directory: no expansion.
		if (extOf(dir)) return [];
		// An empty extension list matches nothing, as the Tauri command does,
		// so folder-input ops (dat, organize) stage the directory itself.
		if (Array.isArray(a.exts) && a.exts.length === 0) return [];
		return [`${dir}/a.nsp`, `${dir}/b.nsp`, `${dir}/c.nsp`];
	},
	cmd_cancel: async (a) => {
		if (typeof a.taskId === "string") cancelled.add(a.taskId);
		return null;
	},

	cmd_read_info: async (a) => {
		const path = typeof a.input === "string" ? a.input : `${FAKE_DIR}/sample.nsp`;
		return JSON.stringify(INFO_SAMPLES[infoKindFor(path)]);
	},

	cmd_run: async (a) => runRun(a),
};

// The unified runner shim. The request is flattened so the fallbacks below read
// the same flat payload the per-op mocks always did.
async function runRun(a: Record<string, unknown>): Promise<unknown> {
	const req = (a.request ?? {}) as Record<string, unknown>;
	const op = String(req.operation ?? "");
	const taskId = typeof a.taskId === "string" ? a.taskId : op;
	const progressKey = typeof a.progressKey === "string" ? a.progressKey : taskId;
	const flat = { ...req, taskId, progressKey, dryRun: req.dry_run };
	if (op.startsWith("dat.")) {
		maybeFail(flat);
		const input = String(req.input ?? FAKE_LIB);
		if (op === "dat.scan") {
			return { ...runOutcome(), comparison: null, data: await datScan(taskId, progressKey, input) };
		}
		if (op === "dat.verify") {
			return { ...runOutcome(), comparison: null, data: await datVerify(taskId, progressKey, input) };
		}
		await fakeProgress(taskId, progressKey, 600);
		return { ...runOutcome(), comparison: null, data: datRename(input, req.dry_run !== false) };
	}
	if (op === "organize") {
		maybeFail(flat);
		const input = String(req.input ?? FAKE_LIB);
		return { ...runOutcome(), comparison: null, data: await organizeRun(taskId, progressKey, input, req.dry_run === true) };
	}
	if (op.endsWith(".verify")) {
		maybeFail(flat);
		await fakeProgress(taskId, progressKey, 900);
		return { ...runOutcome(), comparison: null, data: verifyResult(op.split(".")[0]!) };
	}
	if (op === "hash") {
		maybeFail(flat);
		const options = (req.options ?? {}) as Record<string, unknown>;
		await fakeProgress(taskId, progressKey, 600);
		const digests = hashDigests(String(options.algo ?? ""));
		const input = String(req.input ?? `${FAKE_DIR}/sample.nsp`);
		const data = options.recursive
			? [
					{ path: `${input}/a.nsp`, digests },
					{ path: `${input}/b.nsp`, digests },
				]
			: digests;
		return { ...runOutcome(), comparison: null, data };
	}
	return convertRun(flat);
}

// Convert-family fallback: emit fake progress on the job's key, then resolve a
// plausible RunOutcome so savings tally.
async function convertRun(a: Record<string, unknown>): Promise<RunOutcome | { message: string }> {
	if (a.dryRun) return { message: "ok" };
	maybeFail(a);
	if (typeof a.taskId === "string") {
		await fakeProgress(a.taskId, typeof a.progressKey === "string" ? a.progressKey : a.taskId);
	}
	return runOutcome();
}

export async function invoke<T = unknown>(cmd: string, args?: InvokeArgs): Promise<T> {
	const a = (args ?? {}) as Record<string, unknown>;
	const handler = handlers[cmd];
	if (handler) return handler(a) as Promise<T>;
	return convertRun(a) as Promise<T>;
}

export function listen<T>(event: string, handler: EventCallback<T>): Promise<UnlistenFn> {
	const set = listeners.get(event) ?? new Set<Listener>();
	set.add(handler as Listener);
	listeners.set(event, set);
	return Promise.resolve(() => {
		set.delete(handler as Listener);
	});
}

export function open(options?: OpenDialogOptions): Promise<string | string[] | null> {
	// One-shot path override so browser-driven tests can pick any fake file.
	const w = globalThis as Record<string, unknown>;
	if (typeof w.__mockOpenPath === "string") {
		const p = w.__mockOpenPath;
		w.__mockOpenPath = undefined;
		return Promise.resolve(options?.multiple === true ? [p] : p);
	}
	if (options?.directory === true) {
		return Promise.resolve(options?.multiple === true ? [FAKE_LIB] : FAKE_LIB);
	}
	const exts = options?.filters?.[0]?.extensions ?? [];
	const archive = new Set(["zip", "7z", "rar", "tar", "tgz", "gz"]);
	const ext = exts.find((e) => e !== "*" && !archive.has(e)) ?? "bin";
	const path = `${FAKE_DIR}/sample.${ext}`;
	return Promise.resolve(options?.multiple === true ? [path] : path);
}

export function save(options?: SaveDialogOptions): Promise<string | null> {
	if (options?.defaultPath) return Promise.resolve(options.defaultPath);
	const ext = options?.filters?.[0]?.extensions?.[0] ?? "bin";
	return Promise.resolve(`${FAKE_DIR}/out.${ext}`);
}

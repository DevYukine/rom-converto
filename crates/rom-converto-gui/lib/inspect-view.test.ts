import { describe, expect, it } from "vitest";
import { buildInspectView, wupEncryption } from "./inspect-view";
import type { InfoResult } from "~/types/info";

function view(info: Partial<InfoResult> & { kind: InfoResult["kind"] }) {
	return buildInspectView(info as InfoResult);
}

function row(fields: { label: string; value: string }[], label: string) {
	return fields.find((f) => f.label === label)?.value;
}

describe("buildInspectView ctr", () => {
	it("lists NCSD partitions ahead of CIA contents", () => {
		const v = view({
			kind: "ctr",
			format: "ncsd",
			physical_bytes: 1024,
			title_id: "0004000000000000",
			program_id: "",
			product_code: "CTR-P-TEST",
			maker_code: "01",
			maker_name: null,
			cartridge_size: null,
			ncch_encrypted: false,
			smdh: null,
			icon: null,
			small_icon: null,
			compressed: false,
			ncsd_partitions: [{ index: 0, name: "Game", offset: 0x4000, size: 2048 }],
			cia_contents: [{ index: 0, content_id: "0000000a", size: 512, encrypted: true }],
		});
		expect(v.innerTitle).toBe("Partitions");
		expect(v.innerFiles).toEqual([{ name: "Game", detail: "2.0 KiB · 0x4000" }]);
	});

	it("falls back to CIA contents and flags encrypted entries", () => {
		const v = view({
			kind: "ctr",
			format: "cia",
			physical_bytes: 1024,
			title_id: "0004000000000000",
			program_id: "",
			product_code: "",
			maker_code: "01",
			maker_name: null,
			cartridge_size: null,
			ncch_encrypted: true,
			smdh: null,
			icon: null,
			small_icon: null,
			compressed: false,
			ncsd_partitions: [],
			cia_contents: [{ index: 1, content_id: "0000000a", size: 512, encrypted: true }],
		});
		expect(v.innerTitle).toBe("Contents");
		expect(v.innerFiles).toEqual([{ name: "Content 1", detail: "0000000a · 512 B · encrypted" }]);
	});
});

describe("buildInspectView dol", () => {
	it("lists the FST root and summarizes the remainder", () => {
		const v = view({
			kind: "dol",
			physical_bytes: 4096,
			container: "ISO",
			game_id: "GALE01",
			maker_code: "01",
			maker_name: null,
			disc_number: 0,
			disc_version: 0,
			audio_streaming: false,
			game_name: "Test",
			region: "NTSC-U",
			apploader_date: null,
			banner: null,
			banner_image: null,
			fst_root: [
				{ name: "opening.bnr", size: 1024, is_dir: false },
				{ name: "audio", size: 0, is_dir: true },
			],
			fst_file_count: 8,
			fst_dir_count: 3,
		});
		expect(v.innerTitle).toBe("Disc Files");
		expect(v.innerFiles).toEqual([
			{ name: "opening.bnr", detail: "1.0 KiB" },
			{ name: "audio", detail: "dir" },
			{ name: "8 files, 3 dirs", detail: "" },
		]);
	});
});

describe("buildInspectView nx", () => {
	it("uses the hex twin for the title id", () => {
		const v = view({
			kind: "nx",
			container_kind: "nsp",
			is_compressed: false,
			distribution: "digital",
			structure: "scene",
			physical_bytes: 2048,
			files: [{ partition: null, name: "test.nca", abs_offset: 0, size: 1024 }],
			nca_names: ["test.nca"],
			cnmt_nca_names: [],
			tickets: [],
			xci_partitions: null,
			full: {
				application_title_id: 0,
				application_title_id_hex: "01ABCDEF01234801",
				title_version: 0,
				title_kind: "application",
				storage_id: 2,
				attributes: 0,
				required_system_version: 0,
				required_application_version: null,
				base_application_id: null,
				base_application_id_hex: null,
				content_count: 2,
				total_content_size: 2048,
				contents: [{ content_id: "0123456789abcdef0123", content_type: "Program", size: 1024 }],
				related_titles: [],
				control: null,
			},
		});
		expect(row(v.rom, "Title ID")).toBe("01ABCDEF01234801");
		expect(v.innerFiles).toEqual([{ name: "test.nca", detail: "1.0 KiB" }]);
	});

	it("hints at prod.keys when no full metadata was decrypted", () => {
		const v = view({
			kind: "nx",
			container_kind: "xci",
			is_compressed: false,
			distribution: "cartridge",
			structure: "unknown",
			physical_bytes: 2048,
			files: [],
			nca_names: [],
			cnmt_nca_names: [],
			tickets: [],
			xci_partitions: null,
			full: null,
		});
		expect(row(v.rom, "Keys")).toBe("Provide prod.keys to read title, icon, and content metadata");
	});

	it("flags titlekey encryption when tickets are present", () => {
		const v = view({
			kind: "nx",
			container_kind: "nsp",
			is_compressed: false,
			distribution: "digital",
			structure: "scene",
			physical_bytes: 2048,
			files: [],
			nca_names: [],
			cnmt_nca_names: [],
			tickets: [{ file_name: "01020304.tik", rights_id: "deadbeef", master_key_revision: 0 }],
			xci_partitions: null,
			full: null,
		});
		expect(row(v.rom, "Encryption")).toBe("encrypted (titlekey)");
	});

	it("reports ncz containers as decrypted", () => {
		for (const container_kind of ["nsz", "xcz"] as const) {
			const v = view({
				kind: "nx",
				container_kind,
				is_compressed: true,
				distribution: "digital",
				structure: "scene",
				physical_bytes: 2048,
				files: [],
				nca_names: [],
				cnmt_nca_names: [],
				tickets: [{ file_name: "01020304.tik", rights_id: "deadbeef", master_key_revision: 0 }],
				xci_partitions: null,
				full: null,
			});
			expect(row(v.rom, "Encryption")).toBe("decrypted (ncz sections)");
		}
	});
});

describe("buildInspectView wup", () => {
	it("derives Encryption from source_kind", () => {
		expect(wupEncryption("disc (Test Game)")).toBe("encrypted");
		expect(wupEncryption("nus")).toBe("encrypted");
		expect(wupEncryption("loadiine")).toBe("decrypted");
		expect(wupEncryption("wua (Test Game)")).toBe("decrypted");
		expect(wupEncryption("something else")).toBeNull();
	});

	it("shows the Encryption row before Version", () => {
		const v = view({
			kind: "wup",
			title_id_hex: "0005000010101010",
			title_type: "Game",
			title_version: 0,
			source_kind: "disc (Test Game)",
			meta: null,
			disc_partitions: [],
			bundled_titles: [],
			access_rights: 0,
			group_id: 0,
			total_content_size: 0,
			content_count: 0,
		});
		expect(row(v.rom, "Encryption")).toBe("encrypted");
	});
});

describe("buildInspectView ps3", () => {
	it("shows the Encryption row reflecting the probe verdict", () => {
		const v = view({
			kind: "ps3",
			title_id: "BLES00000",
			encrypted: true,
			encrypted_sectors: 10,
			region_count: 1,
			total_sectors: 100,
			root_files: [],
		});
		expect(row(v.rom, "Encryption")).toBe("encrypted");
	});

	it("omits the Encryption row when the probe couldn't decide", () => {
		const v = view({
			kind: "ps3",
			title_id: "BLES00000",
			encrypted: null,
			encrypted_sectors: 10,
			region_count: 1,
			total_sectors: 100,
			root_files: [],
		});
		expect(row(v.rom, "Encryption")).toBeUndefined();
	});
});

describe("buildInspectView nds", () => {
	it("shows the secure area state and CRC validity", () => {
		const v = view({
			kind: "nds",
			physical_bytes: 0x200000,
			game_title: "TEST GAME",
			game_code: "ATSE",
			maker_code: "01",
			unit_code: 0,
			unit_code_name: "NDS",
			region: 0,
			rom_version: 0,
			device_capacity: 7,
			capacity_bytes: 0x200000,
			ntr_rom_size: 0x200000,
			arm9: { rom_offset: 0x4000, entry_address: 0x2000000, load_address: 0x2000000, size: 0x40000 },
			arm7: { rom_offset: 0x8000, entry_address: 0x2380000, load_address: 0x2380000, size: 0x30000 },
			fnt_offset: 0,
			fnt_size: 0,
			fat_offset: 0,
			fat_size: 0,
			header_crc16: 0xabcd,
			header_crc16_computed: 0xabcd,
			header_crc16_valid: true,
			secure_area: "decrypted",
			banner: null,
		});
		expect(row(v.rom, "Encryption")).toBe("decrypted");
		expect(row(v.rom, "Header CRC16")).toBe("0xABCD (valid)");
		expect(v.innerTitle).toBe("ARM Binaries");
		expect(v.innerFiles).toEqual([
			{ name: "ARM9", detail: "256 KiB · entry 0x02000000" },
			{ name: "ARM7", detail: "192 KiB · entry 0x02380000" },
		]);
	});

	it("flags an invalid CRC with the computed value", () => {
		const v = view({
			kind: "nds",
			physical_bytes: 0x200000,
			game_title: "TEST GAME",
			game_code: "ATSE",
			maker_code: "01",
			unit_code: 0,
			unit_code_name: "NDS",
			region: 0,
			rom_version: 0,
			device_capacity: 7,
			capacity_bytes: 0x200000,
			ntr_rom_size: 0x200000,
			arm9: { rom_offset: 0, entry_address: 0, load_address: 0, size: 0 },
			arm7: { rom_offset: 0, entry_address: 0, load_address: 0, size: 0 },
			fnt_offset: 0,
			fnt_size: 0,
			fat_offset: 0,
			fat_size: 0,
			header_crc16: 0x0001,
			header_crc16_computed: 0x0002,
			header_crc16_valid: false,
			secure_area: "not_present",
			banner: null,
		});
		expect(row(v.rom, "Encryption")).toBe("not present");
		expect(row(v.rom, "Header CRC16")).toBe("0x0001 (invalid, computed 0x0002)");
	});
});

describe("buildInspectView retro", () => {
	it("reports the SNES checksum and region", () => {
		const v = view({
			kind: "retro",
			file_size: 0x8000,
			details: {
				system: "snes",
				mapping: "LoROM",
				copier_header: false,
				header_offset: 0x7fc0,
				title: "TEST CARTRIDGE",
				map_mode: 0x20,
				fastrom: false,
				chipset: 0,
				coprocessor: null,
				rom_size_kb: 1024,
				sram_size_kb: 8,
				country: 1,
				region: "USA and Canada",
				licensee: 0x33,
				version: 0,
				checksum: 0x1234,
				checksum_complement: 0xedcb,
				computed_checksum: 0x5678,
				checksum_valid: false,
			},
		});
		expect(row(v.rom, "Title")).toBe("TEST CARTRIDGE");
		expect(row(v.rom, "System")).toBe("SNES");
		expect(row(v.rom, "Checksum")).toBe("0x1234 (invalid, computed 0x5678)");
	});

	it("omits the Title row for systems with no header title", () => {
		const v = view({
			kind: "retro",
			file_size: 0x8000,
			details: {
				system: "nes",
				nes2: false,
				prg_rom_bytes: 32 * 1024,
				chr_rom_bytes: 8 * 1024,
				mapper: 1,
				submapper: null,
				mirroring: "horizontal",
				battery: false,
				trainer: false,
				four_screen: false,
				console_type: "Nintendo Entertainment System",
				timing: "NTSC",
				prg_ram_bytes: null,
				prg_nvram_bytes: null,
				chr_ram_bytes: null,
				chr_nvram_bytes: null,
			},
		});
		expect(row(v.rom, "Title")).toBeUndefined();
		expect(row(v.rom, "System")).toBe("NES");
		expect(row(v.rom, "Format")).toBe("iNES");
	});

	it("reports the Sega 32X title from the shared Mega Drive header", () => {
		const v = view({
			kind: "retro",
			file_size: 0x8000,
			details: {
				system: "sega32x",
				format: "Raw",
				console: "SEGA 32X",
				copyright: "(C)TEST 1994.NOV",
				domestic_title: "DOMESTIC 32X",
				overseas_title: "OVERSEAS 32X",
				serial: "GM 00001009-00",
				device_support: ["3-button controller"],
				region: ["Japan", "Americas"],
				rom_start: 0,
				rom_end: 0x7fff,
				checksum: 0x1234,
				computed_checksum: 0x1234,
				checksum_valid: true,
			},
		});
		expect(row(v.rom, "Title")).toBe("OVERSEAS 32X");
		expect(row(v.rom, "System")).toBe("Sega 32X");
		expect(row(v.rom, "Console")).toBe("SEGA 32X");
		expect(row(v.rom, "Checksum")).toBe("0x1234 (valid)");
	});

	it("reports the FDS first disk side", () => {
		const v = view({
			kind: "retro",
			file_size: 0x1FDD8,
			details: {
				system: "fds",
				fwnes_header: true,
				side_count: 2,
				sides: [
					{
						licensee_code: 1,
						game_name: "TST",
						game_type_code: 0x20,
						game_type: "Normal disk",
						version: 2,
						side_number: 0,
						disk_number: 0,
						disk_type_code: 0,
						disk_type: "FMC (normal card)",
						boot_read_file_code: 5,
						manufacture_date_raw: "610401",
						manufacture_date: "1986-04-01",
					},
				],
			},
		});
		expect(row(v.rom, "Title")).toBe("TST");
		expect(row(v.rom, "System")).toBe("Famicom Disk System");
		expect(row(v.rom, "Sides")).toBe("2");
		expect(row(v.rom, "Manufacture Date")).toBe("1986-04-01");
	});

	it("reports the Sega Saturn IP header", () => {
		const v = view({
			kind: "retro",
			file_size: 0x800,
			details: {
				system: "sega_saturn",
				sector_size: 2048,
				hardware_id: "SEGA SEGASATURN",
				maker_id: "SEGA TP T-000",
				product_number: "T-0001G",
				version: "V1.000",
				release_date: "19950301",
				device_info: "CD-1/1",
				area_symbols: "JTUE",
				regions: ["Japan", "Asia NTSC", "North America", "Europe"],
				peripheral_symbols: "JAM",
				peripherals: ["Control pad", "Analog controller", "Mouse"],
				title: "TEST GAME",
			},
		});
		expect(row(v.rom, "Title")).toBe("TEST GAME");
		expect(row(v.rom, "System")).toBe("Sega Saturn");
		expect(row(v.rom, "Product Number")).toBe("T-0001G");
		expect(row(v.rom, "Region")).toBe("Japan, Asia NTSC, North America, Europe");
	});

	it("reports the Sega CD boot sector with no checksum fields", () => {
		const v = view({
			kind: "retro",
			file_size: 0x800,
			details: {
				system: "sega_cd",
				sector_size: 2048,
				hardware_id: "SEGADISCSYSTEM",
				console: "SEGA MEGA DRIVE",
				copyright: "(C)TEST 1993.SEP",
				domestic_title: "DOMESTIC CD",
				overseas_title: "OVERSEAS CD",
				serial: "GM T-00001-00",
				device_support: ["3-button controller", "CD-ROM"],
				region: ["Japan", "Americas", "Europe"],
			},
		});
		expect(row(v.rom, "Title")).toBe("OVERSEAS CD");
		expect(row(v.rom, "System")).toBe("Sega CD");
		expect(row(v.rom, "Serial")).toBe("GM T-00001-00");
		expect(row(v.rom, "Checksum")).toBeUndefined();
	});

	it("reports the Dreamcast IP header and gdi track count", () => {
		const v = view({
			kind: "retro",
			file_size: 0x800,
			details: {
				system: "dreamcast",
				sector_size: 2048,
				hardware_id: "SEGA SEGAKATANA",
				maker_id: "SEGA ENTERPRISES",
				device_info: "1234 GD-ROM1/1",
				area_symbols: "JUE",
				regions: ["Japan", "North America", "Europe"],
				peripherals_raw: "00000E10",
				peripherals: ["VGA box", "Jump pack", "Microphone", "Memory card"],
				product_number: "T-00001N",
				version: "V1.002",
				release_date: "20000317",
				boot_filename: "1ST_READ.BIN",
				maker_name: "TEST PUBLISHER",
				title: "TEST GAME",
				gdi: { track_count: 4, tracks: [] },
			},
		});
		expect(row(v.rom, "Title")).toBe("TEST GAME");
		expect(row(v.rom, "System")).toBe("Dreamcast");
		expect(row(v.rom, "Maker")).toBe("TEST PUBLISHER");
		expect(row(v.rom, "GDI Tracks")).toBe("4");
	});
});

describe("buildInspectView pbp", () => {
	it("notes the encrypted PSN image for an NPUMDIMG PSAR and lists present segments", () => {
		const v = view({
			kind: "pbp",
			physical_bytes: 4096,
			version: 0x10000,
			title: "TEST GAME",
			disc_id: "ULUS12345",
			disc_version: "1.00",
			category: "MG",
			category_label: "Game",
			psp_system_ver: "6.61",
			parental_level: 1,
			region: null,
			icon: null,
			segments: [
				{ name: "PARAM.SFO", offset: 0, size: 200, present: true },
				{ name: "ICON1.PMF", offset: 200, size: 0, present: false },
				{ name: "DATA.PSAR", offset: 400, size: 2048, present: true },
			],
			psar_kind: { kind: "npumdimg" },
		});
		expect(row(v.rom, "DATA.PSAR")).toBe("NPUMDIMG (encrypted PSN image; DATA.PSAR is extracted as stored)");
		expect(v.innerTitle).toBe("Segments");
		expect(v.innerFiles).toEqual([
			{ name: "PARAM.SFO", detail: "200 B · 0x00000000" },
			{ name: "DATA.PSAR", detail: "2.0 KiB · 0x00000190" },
		]);
	});
});

describe("buildInspectView vpk", () => {
	it("shows title, category and file count", () => {
		const v = view({
			kind: "vpk",
			title: "TEST VITA GAME",
			title_id: "PCSE00000",
			content_id: "EP0000-PCSE00000_00-0000000000000000",
			app_ver: "01.00",
			category: "gd",
			category_label: "Game",
			icon: null,
			file_count: 42,
			total_size: 1024 * 1024,
		});
		expect(row(v.rom, "Title")).toBe("TEST VITA GAME");
		expect(row(v.rom, "Content Type")).toBe("Game");
		expect(row(v.rom, "Files")).toBe("42");
	});
});

describe("buildInspectView pkg", () => {
	it("shows content type, size and data range", () => {
		const v = view({
			kind: "pkg",
			content_id: "EP0000-PCSE00000_00-0000000000000000",
			pkg_revision: 1,
			pkg_type: 1,
			content_type: 21,
			content_type_label: "Game",
			category: null,
			title: "TEST VITA GAME",
			title_id: "PCSE00000",
			item_count: 10,
			total_size: 2048,
			data_offset: 0x1000,
			data_size: 1024,
			key_type: 2,
			drm_type: null,
			package_flags: null,
			meta_ids: [],
		});
		expect(row(v.rom, "Content Type")).toBe("Game");
		expect(row(v.rom, "Data")).toBe("1.0 KiB @ 0x00001000");
	});
});

describe("buildInspectView chd", () => {
	it("combines pregap, postgap, and subtype into the track detail", () => {
		const v = view({
			kind: "chd",
			version: 5,
			compressors: ["cdlz"],
			hunk_bytes: 1024,
			unit_bytes: 2448,
			hunk_count: 1,
			logical_bytes: 1024,
			physical_bytes: 512,
			compression_ratio: 50,
			raw_sha1: "",
			sha1: "",
			parent_sha1: null,
			tracks: [
				{
					number: 1,
					track_type: "MODE1_RAW",
					frames: 150,
					pregap: 150,
					subtype: "RW",
					pgtype: "MODE1",
					pgsub: "RW_RAW",
					postgap: 75,
				},
			],
			metadata_tags: [],
			version_string: null,
			dvd: null,
			content: null,
		});
		expect(v.innerFiles).toEqual([
			{
				name: "Track 1",
				detail: "MODE1_RAW · 150 frames · pregap 150 (MODE1/RW_RAW) · postgap 75 · sub RW",
			},
		]);
	});
});

import type { RetroDetails } from "~/types/info";
import { add, crcField, formatBytes, hex } from "./shared";
import type { InspectField, KindModule } from "./types";

type RetroSystem = RetroDetails["system"];
type DetailsOf<S extends RetroSystem> = Extract<RetroDetails, { system: S }>;

interface RetroSystemDef<S extends RetroSystem> {
	name: string;
	/** Physical medium; disc systems also badge as DISC instead of ROM. */
	media?: string;
	title?(d: DetailsOf<S>): string | undefined;
	fields?(rom: InspectField[], d: DetailsOf<S>): void;
}

function segaCartFields(rom: InspectField[], d: DetailsOf<"mega_drive" | "sega32x">) {
	add(rom, "Domestic Title", d.domestic_title);
	add(rom, "Serial", d.serial);
	add(rom, "Console", d.console);
	add(rom, "Region", d.region.join(", "));
	add(rom, "Device Support", d.device_support.join(", "));
	add(rom, "Copyright", d.copyright);
	add(rom, "Format", d.format);
	add(rom, "ROM Range", `0x${hex(d.rom_start, 8)}–0x${hex(d.rom_end, 8)}`);
	crcField(rom, "Checksum", d.checksum, d.computed_checksum, d.checksum_valid, 4);
}

function segaHandheldFields(rom: InspectField[], d: DetailsOf<"master_system" | "game_gear">) {
	add(rom, "Region", d.region);
	add(rom, "Product Code", d.product_code);
	add(rom, "Version", d.version);
	if (d.rom_size_kb) add(rom, "ROM Size", `${d.rom_size_kb} KiB`);
	crcField(rom, "Checksum", d.checksum, d.computed_checksum, d.checksum_valid, 4);
}

function segaDiscFields(rom: InspectField[], d: DetailsOf<"sega_saturn" | "dreamcast">) {
	add(rom, "Product Number", d.product_number);
	add(rom, "Version", d.version);
	add(rom, "Release Date", d.release_date);
	add(rom, "Device Info", d.device_info);
	add(rom, "Region", d.regions.join(", "));
	add(rom, "Peripherals", d.peripherals.join(", "));
}

// One entry per console: its display name, its title field, and the rows it adds
// after the shared Title / Content Type / System / Size block.
const RETRO_SYSTEMS: { [S in RetroSystem]: RetroSystemDef<S> } = {
	nes: {
		name: "NES",
		fields(rom, d) {
			add(rom, "Format", d.nes2 ? "NES 2.0" : "iNES");
			add(rom, "Mapper", d.submapper != null ? `${d.mapper}.${d.submapper}` : d.mapper);
			add(rom, "Console Type", d.console_type);
			add(rom, "Timing", d.timing);
			add(rom, "Mirroring", d.four_screen ? "four-screen" : d.mirroring);
			add(rom, "PRG ROM", formatBytes(d.prg_rom_bytes));
			add(rom, "CHR ROM", formatBytes(d.chr_rom_bytes));
			if (d.prg_ram_bytes) add(rom, "PRG RAM", formatBytes(d.prg_ram_bytes));
			if (d.prg_nvram_bytes) add(rom, "PRG NVRAM", formatBytes(d.prg_nvram_bytes));
			if (d.chr_ram_bytes) add(rom, "CHR RAM", formatBytes(d.chr_ram_bytes));
			if (d.chr_nvram_bytes) add(rom, "CHR NVRAM", formatBytes(d.chr_nvram_bytes));
			add(rom, "Battery", d.battery ? "yes" : "no");
			add(rom, "Trainer", d.trainer ? "yes" : "no");
		},
	},
	snes: {
		name: "SNES",
		title: (d) => d.title,
		fields(rom, d) {
			add(rom, "Mapping", d.mapping);
			add(rom, "Region", d.region);
			add(rom, "FastROM", d.fastrom ? "yes" : "no");
			add(rom, "Chipset", `0x${hex(d.chipset, 2)}`);
			add(rom, "Coprocessor", d.coprocessor);
			add(rom, "ROM Size", `${d.rom_size_kb} KiB`);
			add(rom, "SRAM Size", `${d.sram_size_kb} KiB`);
			add(rom, "Licensee", `0x${hex(d.licensee, 2)}`);
			add(rom, "Version", d.version);
			add(rom, "Copier Header", d.copier_header ? "yes" : "no");
			crcField(rom, "Checksum", d.checksum, d.computed_checksum, d.checksum_valid, 4);
		},
	},
	n64: {
		name: "Nintendo 64",
		title: (d) => d.internal_name,
		fields(rom, d) {
			add(rom, "Game ID", d.game_id);
			add(rom, "Media", d.media);
			add(rom, "Region", d.region ?? d.region_code);
			add(rom, "Version", d.version);
			add(rom, "Byte Order", d.byte_order.toUpperCase());
			add(rom, "CIC", d.cic);
			add(rom, "CRC1", d.crc1);
			add(rom, "CRC2", d.crc2);
			add(rom, "Bootcode CRC32", d.bootcode_crc32);
		},
	},
	game_boy: {
		name: "Game Boy",
		title: (d) => d.title,
		fields(rom, d) {
			add(rom, "Mode", d.cgb ?? (d.sgb_flag === 0x03 ? "SGB" : "DMG"));
			add(rom, "Cart Type", d.cart_type_name ?? `0x${hex(d.cart_type, 2)}`);
			if (d.rom_bytes) add(rom, "ROM Size", formatBytes(d.rom_bytes));
			if (d.ram_bytes) add(rom, "RAM Size", formatBytes(d.ram_bytes));
			add(rom, "Destination", d.destination_name);
			add(rom, "Publisher", d.licensee);
			add(rom, "Manufacturer Code", d.manufacturer_code);
			add(rom, "Version", d.version);
			add(rom, "Logo Valid", d.logo_valid ? "yes" : "no");
			crcField(rom, "Header Checksum", d.header_checksum, d.computed_header_checksum, d.header_checksum_valid, 2);
			crcField(rom, "Global Checksum", d.global_checksum, d.computed_global_checksum, d.global_checksum_valid, 4);
		},
	},
	gba: {
		name: "Game Boy Advance",
		title: (d) => d.title,
		fields(rom, d) {
			add(rom, "Game Code", d.game_code);
			add(rom, "Region", d.region);
			add(rom, "Maker", d.maker_code);
			add(rom, "Version", d.version);
			add(rom, "Logo Valid", d.logo_valid ? "yes" : "no");
			crcField(rom, "Header Checksum", d.header_checksum, d.computed_header_checksum, d.header_checksum_valid, 2);
		},
	},
	mega_drive: {
		name: "Mega Drive / Genesis",
		title: (d) => d.overseas_title || d.domestic_title,
		fields: segaCartFields,
	},
	master_system: { name: "Master System", fields: segaHandheldFields },
	game_gear: { name: "Game Gear", fields: segaHandheldFields },
	virtual_boy: {
		name: "Virtual Boy",
		title: (d) => d.title,
		fields(rom, d) {
			add(rom, "Maker", d.maker_code);
			add(rom, "Game Code", d.game_code);
			add(rom, "Version", d.version);
		},
	},
	wonder_swan: {
		name: "WonderSwan",
		fields(rom, d) {
			add(rom, "Publisher ID", d.publisher_id);
			add(rom, "Game ID", d.game_id);
			add(rom, "Color", d.color ? "color" : "mono");
			add(rom, "Save", d.save);
			add(rom, "Version", d.version);
			crcField(rom, "Checksum", d.checksum, d.computed_checksum, d.checksum_valid, 4);
		},
	},
	neo_geo_pocket: {
		name: "Neo Geo Pocket",
		title: (d) => d.title,
		fields(rom, d) {
			add(rom, "License", d.license);
			add(rom, "Machine", d.machine_name);
			add(rom, "Catalog ID", d.catalog_id);
			add(rom, "Subcatalog ID", d.subcatalog_id);
			add(rom, "Startup Address", `0x${hex(d.startup_address, 8)}`);
		},
	},
	lynx: {
		name: "Atari Lynx",
		title: (d) => d.cart_name,
		fields(rom, d) {
			add(rom, "Manufacturer", d.manufacturer);
			add(rom, "Rotation", d.rotation_name ?? String(d.rotation));
			add(rom, "Bank 0 Page Size", d.bank0_page_size);
			add(rom, "Bank 1 Page Size", d.bank1_page_size);
			add(rom, "Version", d.version);
		},
	},
	atari7800: {
		name: "Atari 7800",
		title: (d) => d.title,
		fields(rom, d) {
			add(rom, "TV Type", d.tv_type);
			add(rom, "Cart Size", formatBytes(d.cart_size));
			add(rom, "Cart Type", `0x${hex(d.cart_type, 4)}`);
			add(rom, "Cart Features", d.cart_features.join(", "));
			add(rom, "Controller 1", d.controller1_name);
			add(rom, "Controller 2", d.controller2_name);
			add(rom, "Save Device", d.save_device);
			add(rom, "Version", d.version);
		},
	},
	sega32x: {
		name: "Sega 32X",
		title: (d) => d.overseas_title || d.domestic_title,
		fields: segaCartFields,
	},
	fds: {
		name: "Famicom Disk System",
		title: (d) => d.sides[0]?.game_name,
		fields(rom, d) {
			add(rom, "Format", d.fwnes_header ? "fwNES" : "Headerless");
			add(rom, "Sides", d.side_count);
			const side = d.sides[0];
			if (side) {
				add(rom, "Game Type", side.game_type ?? `0x${hex(side.game_type_code, 2)}`);
				add(rom, "Disk Type", side.disk_type ?? `0x${hex(side.disk_type_code, 2)}`);
				add(rom, "Version", side.version);
				add(rom, "Manufacture Date", side.manufacture_date ?? side.manufacture_date_raw);
			}
		},
	},
	sega_saturn: {
		name: "Sega Saturn",
		media: "CD",
		title: (d) => d.title,
		fields(rom, d) {
			add(rom, "Maker", d.maker_id);
			segaDiscFields(rom, d);
		},
	},
	sega_cd: {
		name: "Sega CD",
		media: "CD",
		title: (d) => d.overseas_title || d.domestic_title,
		fields(rom, d) {
			add(rom, "Domestic Title", d.domestic_title);
			add(rom, "Serial", d.serial);
			add(rom, "Console", d.console);
			add(rom, "Region", d.region.join(", "));
			add(rom, "Device Support", d.device_support.join(", "));
			add(rom, "Copyright", d.copyright);
		},
	},
	dreamcast: {
		name: "Dreamcast",
		media: "GD-ROM",
		title: (d) => d.title,
		fields(rom, d) {
			add(rom, "Maker", d.maker_name || d.maker_id);
			segaDiscFields(rom, d);
			add(rom, "Boot File", d.boot_filename);
			if (d.gdi) add(rom, "GDI Tracks", d.gdi.track_count);
		},
	},
};

function systemDef(d: RetroDetails): RetroSystemDef<RetroSystem> {
	// The map is correlated with `system`; TypeScript can't track that through the union.
	return RETRO_SYSTEMS[d.system] as RetroSystemDef<RetroSystem>;
}

function retroTitle(d: RetroDetails): string | undefined {
	return systemDef(d).title?.(d);
}

export const retro: KindModule<"retro"> = {
	build(info) {
		const d = info.details;
		const def = systemDef(d);
		const rom: InspectField[] = [];
		add(rom, "Title", retroTitle(d));
		add(rom, "Content Type", "Game");
		add(rom, "System", def.name);
		add(rom, "Size", formatBytes(info.file_size));
		def.fields?.(rom, d);
		return { rom };
	},
	title: (info) => retroTitle(info.details) || systemDef(info.details).name,
	size: (info) => info.file_size,
	console: (info) => systemDef(info.details).name.toUpperCase(),
	// Raw disc images all read "DISC"; cartridges read "ROM".
	format: (info) => (systemDef(info.details).media ? "DISC" : "ROM"),
	media: (info) => systemDef(info.details).media ?? null,
	stats: (info) => [{ label: "System", value: systemDef(info.details).name }],
};

import { languageDisplayName } from "../display";
import { add, crcField, englishFirst, formatBytes, hex } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

export const nds: KindModule<"nds"> = {
	build(info): InspectBuild {
		const rom: InspectField[] = [];
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
		return {
			rom,
			innerTitle: "ARM Binaries",
			innerFiles: [
				{ name: "ARM9", detail: `${formatBytes(info.arm9.size)} · entry 0x${hex(info.arm9.entry_address, 8)}` },
				{ name: "ARM7", detail: `${formatBytes(info.arm7.size)} · entry 0x${hex(info.arm7.entry_address, 8)}` },
			],
		};
	},
	title: (info) => englishFirst(info.banner?.titles.entries, (e) => e[0])?.[1] || info.game_title,
	size: (info) => info.physical_bytes,
	console: () => "DS",
	format: () => "NDS",
	meta: (info) => [info.maker_code, info.unit_code_name],
	stats: (info) => [
		{ label: "Game Code", value: info.game_code },
		{
			label: "Encryption",
			value:
				info.secure_area === "not_present" ? "not present" : info.secure_area === "decrypted" ? "decrypted ✓" : "encrypted",
		},
	],
	titleId: (info) => info.game_code,
};

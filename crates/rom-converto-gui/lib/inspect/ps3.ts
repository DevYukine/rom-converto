import { contentTypeDisplayName } from "../display";
import { add, formatBytes } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

export const ps3: KindModule<"ps3"> = {
	build(info): InspectBuild {
		const rom: InspectField[] = [];
		add(rom, "Title", info.title);
		add(rom, "Title ID", info.title_id);
		add(rom, "Content Type", info.content_kind ? contentTypeDisplayName(info.content_kind) : "Game");
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
		return {
			rom,
			innerTitle: "Disc Files",
			innerFiles: info.root_files.map((e) => ({ name: e.name, detail: e.is_dir ? "dir" : formatBytes(e.size) })),
		};
	},
	title: (info) => info.title || info.title_id || "PS3 disc",
	size: (info) => info.size_bytes,
	console: () => "PS3",
	format: () => "DISC",
	media: () => "BD",
	meta: (info) => [info.region, info.version && `v${info.version}`],
	stats: (info) => [
		...(info.title_id ? [{ label: "Title ID", value: info.title_id }] : []),
		...(info.encrypted !== null
			? [{ label: "Encryption", value: info.encrypted ? "encrypted" : "decrypted ✓" }]
			: []),
	],
	titleId: (info) => info.title_id ?? "",
};

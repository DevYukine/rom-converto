import { add, formatBytes, hex } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

function xenonRatio(logicalSize: number, compressedSize: number): number {
	return logicalSize > 0 ? (1 - compressedSize / logicalSize) * 100 : 0;
}

export const xenon: KindModule<"xenon"> = {
	build(info): InspectBuild {
		const container: InspectField[] = [];
		const rom: InspectField[] = [];
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
		const innerFiles = info.root_entries.map((e) => ({ name: e.name, detail: e.is_file ? formatBytes(e.size) : "dir" }));
		if (info.file_count + info.dir_count > info.root_entries.length) {
			innerFiles.push({ name: `${info.file_count} files, ${info.dir_count} dirs`, detail: "" });
		}
		return { container, rom, innerTitle: "Archive Files", innerFiles };
	},
	title: (info) => info.xex?.title_name || "Xbox 360 image",
	size: (info) => info.compressed_size,
	console: () => "XBOX 360",
	format: () => "ZAR",
	stats: (info) => [
		...(info.xex?.title_id_hex ? [{ label: "Title ID", value: info.xex.title_id_hex }] : []),
		{
			label: "Ratio",
			value: `${xenonRatio(info.logical_size, info.compressed_size).toFixed(1)}%`,
			color: "green" as const,
		},
		{ label: "Blocks", value: String(info.block_count) },
	],
	titleId: (info) => (info.xex ? (info.xex.title_id_hex ?? "") : null),
};

import { add, discContentRom, formatBytes } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

export const cso: KindModule<"cso"> = {
	build(info): InspectBuild {
		const container: InspectField[] = [];
		add(container, "Container", `${info.format} v${info.version}`);
		add(container, "Compressed Size", formatBytes(info.physical_bytes));
		add(container, "Logical Size", formatBytes(info.uncompressed_size));
		add(container, "Ratio", `${info.compression_ratio.toFixed(1)}%`);
		add(container, "Block Size", formatBytes(info.block_size));
		add(container, "Blocks", `${info.block_count} (${info.raw_block_count} raw)`);
		add(container, "Index Shift", info.index_shift);
		return { container, rom: info.content ? discContentRom(info.content) : [] };
	},
	title(info) {
		const fallback = `${info.format} image`;
		if (info.content?.kind === "psp") return info.content.title || info.content.title_id || fallback;
		if (info.content?.kind === "psx") return info.content.volume_id || info.content.title_id || fallback;
		return fallback;
	},
	size: (info) => info.physical_bytes,
	console(info) {
		if (info.content?.kind === "psx") return info.content.console;
		if (info.content?.kind === "psp") return "PSP";
		return "CSO";
	},
	format: (info) => info.format.toUpperCase(),
	media(info) {
		if (info.content?.kind === "psx") return info.content.media;
		if (info.content?.kind === "psp") return "UMD";
		return null;
	},
	meta: (info) =>
		info.content
			? [info.content.title_id, info.content.version && `v${info.content.version}`]
			: [`block ${info.block_size}`],
	stats: (info) => [
		...(info.content?.title_id ? [{ label: "Title ID", value: info.content.title_id }] : []),
		{ label: "Ratio", value: `${info.compression_ratio.toFixed(1)}%`, color: "green" as const },
		{ label: "Blocks", value: String(info.block_count) },
	],
};

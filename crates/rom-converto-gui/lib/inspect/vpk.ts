import { contentTypeDisplayName } from "../display";
import { add, formatBytes } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

export const vpk: KindModule<"vpk"> = {
	build(info): InspectBuild {
		const rom: InspectField[] = [];
		add(rom, "Title", info.title);
		add(rom, "Title ID", info.title_id);
		add(rom, "Content Type", info.content_kind ? contentTypeDisplayName(info.content_kind) : (info.category_label ?? info.category ?? "Game"));
		add(rom, "Content ID", info.content_id);
		add(rom, "Version", info.app_ver);
		add(rom, "Size", formatBytes(info.total_size));
		add(rom, "Files", info.file_count);
		return { rom };
	},
	title: (info) => info.title || info.title_id || "Vita package",
	size: (info) => info.total_size,
	console: () => "VITA",
	format: () => "VPK",
	meta: (info) => [
		info.content_kind ? contentTypeDisplayName(info.content_kind) : (info.category_label ?? info.category),
		info.app_ver && `v${info.app_ver}`,
	],
	stats: (info) => [
		...(info.title_id ? [{ label: "Title ID", value: info.title_id }] : []),
		{ label: "Files", value: String(info.file_count) },
	],
	titleId: (info) => info.title_id ?? "",
};

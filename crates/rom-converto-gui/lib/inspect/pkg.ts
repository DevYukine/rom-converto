import { contentTypeDisplayName } from "../display";
import type { PkgInfo } from "~/types";
import { add, formatBytes, hex } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

const PKG_PLATFORM_LABEL: Record<PkgInfo["platform"], string> = {
	ps3: "PS3",
	psp: "PSP",
	vita: "PS Vita",
};

export function pkgPlatformBadge(platform: PkgInfo["platform"]): string {
	return PKG_PLATFORM_LABEL[platform] ?? "PKG";
}

export const pkg: KindModule<"pkg"> = {
	build(info): InspectBuild {
		const rom: InspectField[] = [];
		add(rom, "Title", info.title);
		add(rom, "Title ID", info.title_id);
		add(
			rom,
			"Content Type",
			info.content_kind ? contentTypeDisplayName(info.content_kind) : (info.content_type_label ?? info.category ?? "Game"),
		);
		add(rom, "Content ID", info.content_id);
		add(rom, "Size", formatBytes(info.total_size));
		add(rom, "Items", info.item_count);
		add(rom, "Package Revision", info.pkg_revision);
		add(rom, "Package Type", info.pkg_type);
		add(rom, "Key Type", info.key_type);
		if (info.drm_type != null) add(rom, "DRM Type", info.drm_type);
		if (info.package_flags != null) add(rom, "Package Flags", `0x${hex(info.package_flags, 8)}`);
		add(rom, "Data", `${formatBytes(info.data_size)} @ 0x${hex(info.data_offset, 8)}`);
		if (info.meta_ids.length) add(rom, "Meta IDs", info.meta_ids.map((id) => `0x${hex(id, 8)}`).join(", "));
		return { rom };
	},
	title: (info) => info.title || info.title_id || "Vita package",
	size: (info) => info.total_size,
	console: (info) => pkgPlatformBadge(info.platform),
	format: () => "PKG",
	meta: (info) => [
		info.content_kind ? contentTypeDisplayName(info.content_kind) : (info.content_type_label ?? info.category),
	],
	stats: (info) => [
		...(info.title_id ? [{ label: "Title ID", value: info.title_id }] : []),
		{ label: "Items", value: String(info.item_count) },
	],
	titleId: (info) => info.title_id ?? "",
};

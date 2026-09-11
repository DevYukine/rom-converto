import { contentTypeDisplayName } from "../display";
import { add, contentFlagsLabel, formatBytes, hex, versionDateLabel } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

export const ps4Pkg: KindModule<"ps4_pkg"> = {
	build(info): InspectBuild {
		const rom: InspectField[] = [];
		add(rom, "Title", info.title);
		add(rom, "Title ID", info.title_id);
		add(
			rom,
			"Content Type",
			info.content_kind
				? contentTypeDisplayName(info.content_kind)
				: (info.category_label ?? info.content_type_label ?? "Game"),
		);
		add(
			rom,
			"Category",
			info.category_label && info.category ? `${info.category_label} (${info.category})` : (info.category_label ?? info.category),
		);
		add(rom, "Content ID", info.content_id);
		add(rom, "App Version", info.app_ver);
		add(rom, "Version", info.version);
		add(rom, "System Version", info.system_ver);
		add(
			rom,
			"App Type",
			info.app_type_label && info.app_type != null ? `${info.app_type_label} (${info.app_type})` : (info.app_type_label ?? info.app_type),
		);
		add(rom, "Parental Level", info.parental_level);
		if (info.ps2_classic) add(rom, "PS2 Classic", info.emu_version != null ? `yes (EMU_VERSION ${info.emu_version})` : "yes");
		add(rom, "Size", formatBytes(info.file_size));
		if (info.package_size !== info.file_size) add(rom, "Package Size", formatBytes(info.package_size));
		add(rom, "Entries", info.entry_count);
		add(rom, "DRM Type", info.drm_type);
		add(rom, "Content Flags", contentFlagsLabel(info.content_flags, info.content_flag_labels));
		add(rom, "Finalized", info.finalized ? "yes" : "no");
		add(rom, "PFS Image", `${formatBytes(info.pfs_image_size)} @ 0x${hex(info.pfs_image_offset, 8)}`);
		add(rom, "Version Date", versionDateLabel(info.version_date));
		return {
			rom,
			innerTitle: "Package entries",
			innerFiles: info.entries.map((e) => ({
				name: e.name ?? `0x${hex(e.id, 4)}`,
				detail: formatBytes(e.size) + (e.encrypted ? ", encrypted" : ""),
			})),
		};
	},
	title: (info) => info.title || info.title_id || "PS4 package",
	size: (info) => info.file_size,
	console: () => "PS4",
	format: () => "PKG",
	meta: (info) => [
		info.content_kind ? contentTypeDisplayName(info.content_kind) : (info.category_label ?? info.content_type_label),
	],
	stats: (info) => [
		...(info.title_id ? [{ label: "Title ID", value: info.title_id }] : []),
		{ label: "Entries", value: String(info.entry_count) },
	],
	titleId: (info) => info.title_id ?? "",
};

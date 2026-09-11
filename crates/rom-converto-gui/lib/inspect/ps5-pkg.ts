import { contentTypeDisplayName } from "../display";
import type { Ps5PkgImage } from "~/types";
import { add, contentFlagsLabel, formatBytes, hex, versionDateLabel } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

const PS5_IMAGE_LABEL: Record<Ps5PkgImage, string> = {
	cnt: "CNT",
	fih: "FIH",
	lih: "LIH",
};

export const ps5Pkg: KindModule<"ps5_pkg"> = {
	build(info): InspectBuild {
		const rom: InspectField[] = [];
		add(rom, "Title", info.title);
		add(rom, "Title ID", info.title_id);
		add(rom, "Content Type", info.content_kind ? contentTypeDisplayName(info.content_kind) : (info.content_type_label ?? "Game"));
		add(rom, "Content ID", info.content_id);
		add(
			rom,
			"Image",
			PS5_IMAGE_LABEL[info.image] + (info.signed === true ? " (retail)" : info.signed === false ? " (debug)" : ""),
		);
		add(rom, "Content Version", info.content_version);
		add(rom, "Target Content Version", info.target_content_version);
		add(rom, "Master Version", info.master_version);
		add(rom, "Required Firmware", info.required_system_version);
		add(rom, "SDK Version", info.sdk_version);
		add(
			rom,
			"Application Category",
			info.application_category_label && info.application_category_type != null
				? `${info.application_category_label} (${info.application_category_type})`
				: (info.application_category_label ?? info.application_category_type),
		);
		add(rom, "DRM", info.application_drm_type);
		add(rom, "Default Language", info.default_language);
		add(rom, "Created", info.creation_date);
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
	title: (info) => info.title || info.title_id || "PS5 package",
	size: (info) => info.file_size,
	console: () => "PS5",
	format: () => "PKG",
	meta: (info) => [info.content_kind ? contentTypeDisplayName(info.content_kind) : info.content_type_label],
	stats: (info) => [
		...(info.title_id ? [{ label: "Title ID", value: info.title_id }] : []),
		{ label: "Entries", value: String(info.entry_count) },
	],
	titleId: (info) => info.title_id ?? "",
};

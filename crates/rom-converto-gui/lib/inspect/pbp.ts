import { contentTypeDisplayName } from "../display";
import type { PsarKind } from "~/types/info";
import { add, formatBytes, hex } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

function psarKindLabel(k: PsarKind): string {
	switch (k.kind) {
		case "npumdimg":
			return "NPUMDIMG (encrypted PSN image; DATA.PSAR is extracted as stored)";
		case "psisoimg":
			return "PSISOIMG";
		case "pstitleimg":
			return "PSTITLEIMG";
		case "unknown":
			return `unknown (magic ${k.magic})`;
	}
}

export const pbp: KindModule<"pbp"> = {
	build(info): InspectBuild {
		const rom: InspectField[] = [];
		add(rom, "Title", info.title);
		add(rom, "Title ID", info.disc_id);
		add(rom, "Content Type", info.content_kind ? contentTypeDisplayName(info.content_kind) : (info.category_label ?? info.category ?? "Game"));
		add(rom, "Version", info.disc_version);
		add(rom, "Size", formatBytes(info.physical_bytes));
		add(rom, "System Version", info.psp_system_ver);
		add(rom, "Parental Level", info.parental_level);
		add(rom, "Region", info.region);
		add(rom, "DATA.PSAR", info.psar_kind ? psarKindLabel(info.psar_kind) : null);
		return {
			rom,
			innerTitle: "Segments",
			innerFiles: info.segments
				.filter((s) => s.present)
				.map((s) => ({ name: s.name, detail: `${formatBytes(s.size)} · 0x${hex(s.offset, 8)}` })),
		};
	},
	title: (info) => info.title || info.disc_id || "PSP image",
	size: (info) => info.physical_bytes,
	console: () => "PSP",
	format: () => "EBOOT.PBP",
	meta: (info) => [
		info.content_kind ? contentTypeDisplayName(info.content_kind) : (info.category_label ?? info.category),
		info.disc_version && `v${info.disc_version}`,
	],
	stats: (info) => [
		...(info.disc_id ? [{ label: "Disc ID", value: info.disc_id }] : []),
		{ label: "Segments", value: String(info.segments.filter((s) => s.present).length) },
	],
	titleId: (info) => info.disc_id ?? "",
};

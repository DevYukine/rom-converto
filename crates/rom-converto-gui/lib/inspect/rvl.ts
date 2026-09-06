import { languageDisplayName } from "../display";
import { add, englishFirst, formatBytes, formatMaker, hex } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

export const rvl: KindModule<"rvl"> = {
	build(info): InspectBuild {
		const container: InspectField[] = [];
		const rom: InspectField[] = [];
		if (info.container.toUpperCase() !== "ISO") {
			add(container, "Container", info.container.toUpperCase());
			add(container, "Compressed Size", formatBytes(info.physical_bytes));
		}
		const tmd = info.tmd;
		add(rom, "Title", englishFirst(info.imet_names?.entries, (e) => e[0])?.[1] || info.game_name);
		add(rom, "Title ID", tmd ? tmd.title_id_hex : info.game_id);
		add(rom, "Content Type", "Game");
		add(rom, "Version", tmd ? `v${tmd.title_version}` : `v${info.disc_version}`);
		add(rom, "Region", info.region);
		add(rom, "Languages", info.imet_names?.entries.map((e) => languageDisplayName(e[0])).join(", "));
		add(rom, "Publisher", formatMaker(info.maker_code, info.maker_name));
		add(rom, "Size", formatBytes(info.physical_bytes));
		add(rom, "Game ID", info.game_id);
		add(rom, "Disc Number", info.disc_number);
		if (tmd) {
			if (tmd.ios_slot != null) add(rom, "IOS", `IOS${tmd.ios_slot}`);
			add(rom, "TMD Region", tmd.region_name);
			add(rom, "TMD Contents", tmd.content_count);
			add(rom, "System Version", `0x${tmd.system_version.toString(16).toUpperCase()}`);
			add(rom, "Access Rights", `0x${hex(tmd.access_rights, 8)}`);
		}
		return {
			container,
			rom,
			innerTitle: "Partitions",
			innerFiles: info.partitions.map((p) => ({
				name: p.kind,
				detail: `type ${p.partition_type} · group ${p.group} · 0x${p.offset.toString(16).toUpperCase()}`,
			})),
		};
	},
	title: (info) => englishFirst(info.imet_names?.entries, (e) => e[0])?.[1] || info.game_name || info.game_id,
	size: (info) => info.physical_bytes,
	console: () => "WII",
	format(info) {
		const container = info.container.toUpperCase();
		return container === "ISO" || container === "GCM" ? "DISC" : container;
	},
	media: () => "DVD",
	meta: (info) => [formatMaker(info.maker_code, info.maker_name), info.region],
	stats: (info) => [
		{ label: "Game ID", value: info.game_id },
		...(info.tmd ? [{ label: "Title ID", value: info.tmd.title_id_hex }] : []),
	],
	titleId: (info) => (info.tmd ? info.tmd.title_id_hex : info.game_id),
};

import { languageDisplayName } from "../display";
import { add, englishFirst, formatBytes, formatMaker } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

export const dol: KindModule<"dol"> = {
	build(info): InspectBuild {
		const container: InspectField[] = [];
		const rom: InspectField[] = [];
		if (info.container.toUpperCase() !== "ISO") {
			add(container, "Container", info.container.toUpperCase());
			add(container, "Compressed Size", formatBytes(info.physical_bytes));
		}
		const banner = englishFirst(info.banner?.titles, (b) => b.language);
		add(rom, "Title", banner?.long_game_name || banner?.short_game_name || info.game_name);
		add(rom, "Title ID", info.game_id);
		add(rom, "Content Type", "Game");
		add(rom, "Version", `v${info.disc_version}`);
		add(rom, "Region", info.region);
		add(rom, "Languages", info.banner?.titles.map((t) => languageDisplayName(t.language)).join(", "));
		add(rom, "Publisher", banner?.long_maker || formatMaker(info.maker_code, info.maker_name));
		add(rom, "Size", formatBytes(info.physical_bytes));
		add(rom, "Disc Number", info.disc_number);
		add(rom, "Apploader Date", info.apploader_date);
		add(rom, "Audio Streaming", info.audio_streaming ? "yes" : "no");
		const innerFiles = info.fst_root.map((e) => ({ name: e.name, detail: e.is_dir ? "dir" : formatBytes(e.size) }));
		if (info.fst_file_count + info.fst_dir_count > info.fst_root.length) {
			innerFiles.push({ name: `${info.fst_file_count} files, ${info.fst_dir_count} dirs`, detail: "" });
		}
		return { container, rom, innerTitle: "Disc Files", innerFiles };
	},
	title(info) {
		const t = englishFirst(info.banner?.titles, (b) => b.language);
		return t?.long_game_name || t?.short_game_name || info.game_name || info.game_id;
	},
	size: (info) => info.physical_bytes,
	console: () => "GAMECUBE",
	format(info) {
		const container = info.container.toUpperCase();
		return container === "ISO" || container === "GCM" ? "DISC" : container;
	},
	media: () => "MiniDVD",
	meta: (info) => [formatMaker(info.maker_code, info.maker_name), info.region],
	stats: (info) => [
		{ label: "Game ID", value: info.game_id },
		{ label: "Disc", value: `#${info.disc_number} v${info.disc_version}` },
	],
	titleId: (info) => info.game_id,
};

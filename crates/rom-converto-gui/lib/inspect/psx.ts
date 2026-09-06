import { discContentRom } from "./shared";
import type { KindModule } from "./types";

export const psx: KindModule<"psx"> = {
	build: (info) => ({ rom: discContentRom(info) }),
	title: (info) => info.volume_id || info.title_id || "PlayStation disc",
	size: (info) => info.size_bytes,
	console: (info) => info.console,
	format: () => "DISC",
	media: (info) => info.media,
	meta: (info) => [info.version && `v${info.version}`],
	titleId: (info) => info.title_id ?? "",
};

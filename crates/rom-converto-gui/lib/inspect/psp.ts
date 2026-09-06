import { contentTypeDisplayName } from "../display";
import { discContentRom } from "./shared";
import type { KindModule } from "./types";

export const psp: KindModule<"psp"> = {
	build: (info) => ({ rom: discContentRom(info) }),
	title: (info) => info.title || info.title_id || "PSP disc",
	size: (info) => info.size_bytes,
	console: () => "PSP",
	format: () => "DISC",
	media: () => "UMD",
	meta: (info) => [
		info.firmware && `fw ${info.firmware}`,
		info.content_kind ? contentTypeDisplayName(info.content_kind) : info.category,
	],
	titleId: (info) => info.title_id ?? "",
};

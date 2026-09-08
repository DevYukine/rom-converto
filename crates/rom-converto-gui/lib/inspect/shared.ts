import { contentTypeDisplayName } from "../display";
import type { DiscContent, LdClvTime } from "~/types";
import type { InspectField } from "./types";

export function add(list: InspectField[], label: string, value: string | number | null | undefined) {
	if (value === null || value === undefined || value === "") return;
	list.push({ label, value: String(value) });
}

export function hex(n: number, width: number): string {
	return n.toString(16).padStart(width, "0").toUpperCase();
}

export function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	const units = ["KiB", "MiB", "GiB", "TiB"];
	let value = n / 1024;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	return `${value.toFixed(value >= 10 ? 0 : 1)} ${units[unit]}`;
}

export function formatMaker(code: string, name: string | null): string {
	return name ? `${code} (${name})` : code;
}

// Language tags differ per format ("AmericanEnglish", "english", "american_english");
// normalize before comparing. Falls back to the first entry.
export function englishFirst<T>(items: T[] | undefined, lang: (item: T) => string): T | undefined {
	if (!items?.length) return undefined;
	for (const pref of ["americanenglish", "english", "britishenglish"]) {
		const hit = items.find((item) => lang(item).replace(/[_\s]/g, "").toLowerCase() === pref);
		if (hit) return hit;
	}
	return items[0];
}

export function crcField(
	rom: InspectField[],
	label: string,
	stored: number,
	computed: number,
	valid: boolean,
	width: number,
) {
	add(rom, label, `0x${hex(stored, width)} (${valid ? "valid" : `invalid, computed 0x${hex(computed, width)}`})`);
}

export function ldClvTime(t: LdClvTime): string {
	return `${t.hours}:${String(t.minutes).padStart(2, "0")}`;
}

export function ldDiscTypeLabel(discType: "cav" | "clv" | "unknown"): string {
	return discType === "unknown" ? "unknown" : discType.toUpperCase();
}

export function discContentRom(content: DiscContent): InspectField[] {
	const rom: InspectField[] = [];
	if (content.kind === "psx") {
		add(rom, "Title", content.volume_id);
		add(rom, "Title ID", content.title_id);
		add(rom, "Content Type", "Game");
		add(rom, "Version", content.version);
		add(rom, "Size", formatBytes(content.size_bytes));
		add(rom, "Media", content.media);
		add(rom, "Boot Executable", content.boot_executable);
		add(rom, "Total Sectors", content.total_sectors);
	} else {
		add(rom, "Title", content.title);
		add(rom, "Title ID", content.title_id);
		add(rom, "Content Type", content.content_kind ? contentTypeDisplayName(content.content_kind) : "Game");
		add(rom, "Version", content.version);
		add(rom, "Size", formatBytes(content.size_bytes));
		add(rom, "Firmware", content.firmware);
		add(rom, "Total Sectors", content.total_sectors);
	}
	return rom;
}

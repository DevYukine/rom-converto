import { describe, expect, it } from "vitest";
import { kindModules } from "./index";
import { formatBytes } from "./shared";
import type { InfoKind, InfoOf } from "./types";

function fixture<K extends InfoKind>(info: Partial<InfoOf<K>> & { kind: K }): InfoOf<K> {
	return info as InfoOf<K>;
}

// One minimal fixture per kind whose module functions tolerate a partial
// payload; retro's discriminated per-system payload is exercised directly in
// ../inspect-view.test.ts instead.
const fixtures: Partial<{ [K in InfoKind]: InfoOf<K> }> = {
	chd: fixture({
		kind: "chd",
		version: 5,
		version_string: null,
		physical_bytes: 512,
		compression_ratio: 50,
		hunk_count: 1,
		content: null,
	}),
	ctr: fixture({
		kind: "ctr",
		format: "cia",
		physical_bytes: 1024,
		title_id: "0004000000000000",
		product_code: "CTR-P-TEST",
		ncch_encrypted: false,
		smdh: null,
		compressed: false,
	}),
	dol: fixture({
		kind: "dol",
		physical_bytes: 4096,
		container: "ISO",
		game_id: "GALE01",
		game_name: "Test",
		disc_number: 0,
		disc_version: 0,
		banner: null,
	}),
	nx: fixture({
		kind: "nx",
		container_kind: "nsp",
		is_compressed: false,
		physical_bytes: 2048,
		nca_names: ["test.nca"],
		full: null,
	}),
	wup: fixture({
		kind: "wup",
		title_id_hex: "0005000010101010",
		source_kind: "disc (Test Game)",
		total_content_size: 1024,
		content_count: 1,
		meta: null,
	}),
	ps3: fixture({
		kind: "ps3",
		title: "TEST GAME",
		title_id: "BLES00000",
		size_bytes: 1024,
		encrypted: true,
	}),
	ntr: fixture({
		kind: "ntr",
		game_title: "TEST GAME",
		game_code: "ATSE",
		physical_bytes: 0x200000,
		secure_area: "decrypted",
		banner: null,
	}),
	psp: fixture({
		kind: "psp",
		title: "TEST PSP GAME",
		title_id: "ULUS12345",
		size_bytes: 1024,
	}),
	pbp: fixture({
		kind: "pbp",
		title: "TEST GAME",
		disc_id: "ULUS12345",
		physical_bytes: 4096,
		segments: [],
	}),
	vpk: fixture({
		kind: "vpk",
		title: "TEST VITA GAME",
		title_id: "PCSE00000",
		total_size: 1024,
		file_count: 3,
	}),
	pkg: fixture({
		kind: "pkg",
		title: "TEST VITA GAME",
		title_id: "PCSE00000",
		total_size: 2048,
		item_count: 10,
		platform: "vita",
	}),
};

describe("kindModules", () => {
	const kinds = Object.keys(fixtures) as InfoKind[];
	// At least 8 kinds ensures the sweep is not trivially satisfied by one or two.
	expect(kinds.length).toBeGreaterThanOrEqual(8);

	for (const kind of kinds) {
		it(`${kind}: title, badges, and stat row are non-empty strings`, () => {
			const mod = kindModules[kind];
			const info = fixtures[kind] as never;

			const title = mod.title(info);
			expect(title).toBeTypeOf("string");
			expect(title.length).toBeGreaterThan(0);

			const formatBadge = mod.format(info);
			expect(formatBadge).toBeTypeOf("string");
			expect(formatBadge.length).toBeGreaterThan(0);

			const consoleBadge = mod.console(info);
			expect(consoleBadge).toBeTypeOf("string");
			expect(consoleBadge.length).toBeGreaterThan(0);

			const statRow = [{ label: "Size", value: formatBytes(mod.size(info)) }, ...(mod.stats?.(info) ?? [])];
			expect(statRow.length).toBeGreaterThan(0);
			for (const stat of statRow) {
				expect(stat.label).toBeTypeOf("string");
				expect(stat.label.length).toBeGreaterThan(0);
				expect(stat.value).toBeTypeOf("string");
			}
		});
	}
});

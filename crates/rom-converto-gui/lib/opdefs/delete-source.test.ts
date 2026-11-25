import { beforeEach, describe, expect, it } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { allOpDefs } from "./index";
import type { StagedItem } from "./types";

const ITEM: StagedItem = {
	id: "item-1",
	path: "/roms/sample.iso",
	name: "sample.iso",
	size: 1024,
	outExt: "chd",
};

const defs = allOpDefs();

describe("delete-source toggle", () => {
	beforeEach(() => setActivePinia(createPinia()));

	// Pages whose operation may replace and delete its input. The Wii U page
	// covers both WUD to WUX and WUX to WUD under one toggle.
	const allowed: [string, string][] = [
		["chd-compress", "/roms/Game A.cue"],
		["cso-decompress", "/roms/game.cso"],
		["rvl-compress", "/roms/game.iso"],
		["ps3-decrypt", "/roms/game.iso"],
		["xbox-convert", "/roms/game.iso"],
		["wup-convert", "/roms/game.wud"],
	];

	it.each(allowed)("%s shows a toggle that sends delete_source", (storeId, path) => {
		const def = defs.find((def) => def.storeId === storeId)!;
		const field = def.fields.find((field) => field.key === "deleteSource");
		expect(field, storeId).toBeDefined();
		expect(field?.kind, storeId).toBe("toggle");

		const store = def.useStore();
		expect(store.deleteSource, storeId).toBe(false);
		store.deleteSource = true;
		const { request } = def.buildArgs(store, { ...ITEM, path }, "task-1");
		expect(request.options.delete_source, storeId).toBe(true);
	});

	// Extraction and cue-to-iso never replace their input, so they get no toggle.
	it.each(["chd-extract", "cue-convert"])("%s has no delete-originals toggle", (storeId) => {
		const def = defs.find((def) => def.storeId === storeId)!;
		expect(def.fields.some((field) => field.key === "deleteSource"), storeId).toBe(false);
	});
});

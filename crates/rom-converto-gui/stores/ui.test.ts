import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { useUiStore } from "./ui";

const STORAGE_KEY = "rom-converto:ui";

// Fresh Pinia per test, plus a deterministic localStorage stub so the store's
// load path sees exactly what each test seeds.
beforeEach(() => {
	setActivePinia(createPinia());
	const data: Record<string, string> = {};
	vi.stubGlobal("localStorage", {
		getItem: (key: string) => data[key] ?? null,
		setItem: (key: string, value: string) => {
			data[key] = value;
		},
		removeItem: (key: string) => {
			delete data[key];
		},
	});
});

afterEach(() => {
	vi.unstubAllGlobals();
});

describe("rememberOutputDir", () => {
	it("dedupes, keeps the newest first, and ignores empty values", () => {
		const ui = useUiStore();
		for (const dir of ["/a", "/b", "/c", "/d", "/e"]) ui.rememberOutputDir(dir);
		ui.rememberOutputDir("/a");
		expect(ui.recentOutputDirs).toEqual(["/a", "/e", "/d", "/c", "/b"]);
		ui.rememberOutputDir("/f");
		expect(ui.recentOutputDirs).toEqual(["/f", "/a", "/e", "/d", "/c"]);
		ui.rememberOutputDir("/f");
		expect(ui.recentOutputDirs).toEqual(["/f", "/a", "/e", "/d", "/c"]);
		ui.rememberOutputDir("");
		expect(ui.recentOutputDirs).toEqual(["/f", "/a", "/e", "/d", "/c"]);
	});
});

describe("persisted recents", () => {
	it("loads only non-empty strings from storage", () => {
		localStorage.setItem(
			STORAGE_KEY,
			JSON.stringify({ recentOutputDirs: ["/kept", 3, "", null, "/kept"] }),
		);
		const ui = useUiStore();
		expect(ui.recentOutputDirs).toEqual(["/kept"]);
	});
});

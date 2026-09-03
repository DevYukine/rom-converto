import { describe, expect, it, vi } from "vitest";
import type { DownloadEvent } from "@tauri-apps/plugin-updater";
import { createUpdater, promptOpen, type UpdateState } from "./updater";

describe("updater", () => {
	it("is safe outside Tauri", async () => {
		const updater = createUpdater(false);
		await updater.checkForUpdate();
		expect(updater.state).toMatchObject({
			phase: "error",
			error: "Update checks require the desktop app.",
		});
	});

	it("reports when the current version is up to date", async () => {
		const updater = createUpdater(true, undefined, async () => ({ check: async () => null, relaunch: vi.fn() }));
		await updater.checkForUpdate();
		expect(updater.state.phase).toBe("up-to-date");
	});

	it("downloads, installs, and relaunches an available update", async () => {
		const relaunch = vi.fn();
		const seen: number[] = [];
		const downloadAndInstall = vi.fn(async (onEvent: (event: DownloadEvent) => void) => {
			onEvent({ event: "Started", data: { contentLength: 400 } });
			onEvent({ event: "Progress", data: { chunkLength: 100 } });
			onEvent({ event: "Progress", data: { chunkLength: 1 } });
			onEvent({ event: "Progress", data: { chunkLength: 299 } });
			onEvent({ event: "Finished" });
		});
		const updater = createUpdater(true, (s) => seen.push(s.progress), async () => ({
			check: async () => ({ version: "2.0.0", downloadAndInstall }),
			relaunch,
		}));

		await updater.checkForUpdate();
		expect(updater.state).toMatchObject({ phase: "available", availableVersion: "2.0.0" });
		await updater.installUpdate();
		expect(downloadAndInstall).toHaveBeenCalledOnce();
		expect(relaunch).toHaveBeenCalledOnce();
		expect(updater.state.phase).toBe("installing");
		// One emit per whole percent, so the 1-byte chunk does not re-emit 25%.
		expect(seen.filter((p, i) => p >= 0 && p !== seen[i - 1])).toEqual([0.25, 1]);
	});

	it("reports an install failure", async () => {
		const updater = createUpdater(true, undefined, async () => ({
			check: async () => ({
				version: "2.0.0",
				downloadAndInstall: async () => {
					throw new Error("signature mismatch");
				},
			}),
			relaunch: vi.fn(),
		}));
		await updater.checkForUpdate();
		await updater.installUpdate();
		expect(updater.state).toMatchObject({ phase: "error", error: "Error: signature mismatch" });
	});

	it("prevents duplicate checks", async () => {
		const check = vi.fn(async () => null);
		const updater = createUpdater(true, undefined, async () => ({ check, relaunch: vi.fn() }));
		await Promise.all([updater.checkForUpdate(), updater.checkForUpdate()]);
		expect(check).toHaveBeenCalledOnce();
	});
});

describe("promptOpen", () => {
	const at = (phase: UpdateState["phase"], availableVersion = "2.0.0"): UpdateState => ({
		phase,
		availableVersion,
		progress: -1,
		error: "",
	});

	it("opens for a found version the user has not hidden", () => {
		expect(promptOpen(at("available"), ["", ""], false)).toBe(true);
	});

	it("stays closed for a dismissed or skipped version", () => {
		expect(promptOpen(at("available"), ["2.0.0", ""], false)).toBe(false);
		expect(promptOpen(at("available"), ["", "2.0.0"], false)).toBe(false);
		expect(promptOpen(at("available", "2.1.0"), ["2.0.0", ""], false)).toBe(true);
	});

	it("stays open through an install and its failure, but not a failed check", () => {
		expect(promptOpen(at("downloading"), ["2.0.0"], true)).toBe(true);
		expect(promptOpen(at("installing"), ["2.0.0"], true)).toBe(true);
		expect(promptOpen(at("error"), [], true)).toBe(true);
		expect(promptOpen(at("error"), [], false)).toBe(false);
	});

	it("holds still during a re-check and closes once up to date", () => {
		expect(promptOpen(at("checking"), [], false)).toBe(true);
		expect(promptOpen(at("checking", ""), [], false)).toBe(false);
		expect(promptOpen(at("up-to-date", ""), [], false)).toBe(false);
	});

	it("never treats an empty version as found", () => {
		expect(promptOpen(at("available", ""), ["", ""], false)).toBe(false);
	});
});

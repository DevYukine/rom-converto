import { describe, expect, it, vi } from "vitest";
import type { DownloadEvent } from "@tauri-apps/plugin-updater";
import { createUpdater } from "./updater";

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
		const downloadAndInstall = vi.fn(async (onEvent: (event: DownloadEvent) => void) =>
			onEvent({ event: "Finished" }),
		);
		const updater = createUpdater(true, undefined, async () => ({
			check: async () => ({ version: "2.0.0", downloadAndInstall }),
			relaunch,
		}));

		await updater.checkForUpdate();
		expect(updater.state).toMatchObject({ phase: "available", availableVersion: "2.0.0" });
		await updater.installUpdate();
		expect(downloadAndInstall).toHaveBeenCalledOnce();
		expect(relaunch).toHaveBeenCalledOnce();
		expect(updater.state.phase).toBe("installing");
	});

	it("prevents duplicate checks", async () => {
		let resolve!: (value: null) => void;
		const check = vi.fn(() => new Promise<null>((done) => (resolve = done)));
		const updater = createUpdater(true, undefined, async () => ({ check, relaunch: vi.fn() }));
		const first = updater.checkForUpdate();
		await updater.checkForUpdate();
		resolve(null);
		await first;
		expect(check).toHaveBeenCalledOnce();
	});
});

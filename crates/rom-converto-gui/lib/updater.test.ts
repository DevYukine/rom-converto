import { describe, expect, it, vi } from "vitest";
import type { UpdateEvent } from "~/types";
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
		const updater = createUpdater(true, undefined, { check: async () => null, install: vi.fn() });
		await updater.checkForUpdate();
		expect(updater.state.phase).toBe("up-to-date");
	});

	it("downloads and installs an available update", async () => {
		const seen: number[] = [];
		const install = vi.fn(async (onEvent: (event: UpdateEvent) => void) => {
			onEvent({ kind: "progress", downloaded: 100, total: 400 });
			// An understated Content-Length must not push the bar past full.
			onEvent({ kind: "progress", downloaded: 500, total: 400 });
			onEvent({ kind: "installing" });
		});
		const updater = createUpdater(true, (s) => seen.push(s.progress), { check: async () => "2.0.0", install });

		await updater.checkForUpdate();
		expect(updater.state).toMatchObject({ phase: "available", availableVersion: "2.0.0" });
		await updater.installUpdate();
		expect(install).toHaveBeenCalledOnce();
		expect(updater.state.phase).toBe("installing");
		expect(seen.filter((p, i) => p >= 0 && p !== seen[i - 1])).toEqual([0.25, 1]);
	});

	it("reports an install failure", async () => {
		const updater = createUpdater(true, undefined, {
			check: async () => "2.0.0",
			install: async () => {
				throw new Error("signature mismatch");
			},
		});
		await updater.checkForUpdate();
		await updater.installUpdate();
		expect(updater.state).toMatchObject({ phase: "error", error: "Error: signature mismatch" });
	});

	it("prevents duplicate checks", async () => {
		const check = vi.fn(async () => null);
		const updater = createUpdater(true, undefined, { check, install: vi.fn() });
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

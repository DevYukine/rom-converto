import type { DownloadEvent } from "@tauri-apps/plugin-updater";

export type UpdatePhase =
	| "current"
	| "checking"
	| "available"
	| "downloading"
	| "installing"
	| "up-to-date"
	| "error";

export interface UpdateState {
	phase: UpdatePhase;
	availableVersion: string;
	/** Download completion in 0..1, or -1 while the size is unknown. */
	progress: number;
	error: string;
}

/** Delay after launch before the first background check, so startup stays responsive. */
export const CHECK_DELAY_MS = 5_000;
/** Interval between background checks while the app stays open. */
export const CHECK_INTERVAL_MS = 4 * 60 * 60 * 1000;

interface PendingUpdate {
	version: string;
	downloadAndInstall(onEvent: (event: DownloadEvent) => void): Promise<void>;
	close?(): Promise<void>;
}

interface UpdaterBridge {
	check(): Promise<PendingUpdate | null>;
	relaunch(): Promise<void>;
}

async function loadBridge(): Promise<UpdaterBridge> {
	const [{ check }, { relaunch }] = await Promise.all([
		import("@tauri-apps/plugin-updater"),
		import("@tauri-apps/plugin-process"),
	]);
	return { check, relaunch };
}

/**
 * Whether the update toast is open. A found version stays hidden once the
 * user dismissed or skipped it; an install the user started stays visible
 * through its progress and any failure so the outcome is never lost.
 */
export function promptOpen(state: UpdateState, hidden: readonly string[], installStarted: boolean): boolean {
	switch (state.phase) {
		// A re-check keeps the previous version until it resolves, so the toast
		// holds still instead of blinking out and back in.
		case "checking":
		case "available":
			return state.availableVersion !== "" && !hidden.includes(state.availableVersion);
		case "downloading":
		case "installing":
			return true;
		case "error":
			return installStarted;
		default:
			return false;
	}
}

export function createUpdater(
	tauri: boolean,
	changed: (state: UpdateState) => void = () => {},
	load: () => Promise<UpdaterBridge> = loadBridge,
) {
	const state: UpdateState = { phase: "current", availableVersion: "", progress: -1, error: "" };
	let bridge: UpdaterBridge | null = null;
	let update: PendingUpdate | null = null;
	const change = (next: Partial<UpdateState>) => {
		Object.assign(state, next);
		changed({ ...state });
	};

	async function checkForUpdate() {
		if (["checking", "downloading", "installing"].includes(state.phase)) return;
		if (!tauri) {
			change({ phase: "error", error: "Update checks require the desktop app." });
			return;
		}

		change({ phase: "checking", error: "" });
		try {
			bridge = await load();
			// Each check allocates a backend resource; release the previous one.
			await update?.close?.();
			update = null;
			update = await bridge.check();
			change({
				phase: update ? "available" : "up-to-date",
				availableVersion: update?.version ?? "",
			});
		} catch (error) {
			change({ phase: "error", error: String(error) });
		}
	}

	async function installUpdate() {
		if (state.phase !== "available" || !bridge || !update) return;
		change({ phase: "downloading", progress: -1 });
		try {
			let total = 0;
			let downloaded = 0;
			let percent = -1;
			await update.downloadAndInstall((event) => {
				if (event.event === "Started") total = event.data.contentLength ?? 0;
				else if (event.event === "Progress" && total > 0) {
					downloaded += event.data.chunkLength;
					const next = Math.min(Math.floor((downloaded / total) * 100), 100);
					if (next !== percent) {
						percent = next;
						change({ progress: next / 100 });
					}
				} else if (event.event === "Finished") change({ phase: "installing" });
			});
			change({ phase: "installing" });
			await bridge.relaunch();
		} catch (error) {
			change({ phase: "error", error: String(error) });
		}
	}

	return { state, checkForUpdate, installUpdate };
}

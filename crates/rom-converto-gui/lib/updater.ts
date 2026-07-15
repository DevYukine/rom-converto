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
	error: string;
}

interface PendingUpdate {
	version: string;
	downloadAndInstall(onEvent: (event: DownloadEvent) => void): Promise<void>;
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

export function createUpdater(
	tauri: boolean,
	changed: (state: UpdateState) => void = () => {},
	load: () => Promise<UpdaterBridge> = loadBridge,
) {
	const state: UpdateState = { phase: "current", availableVersion: "", error: "" };
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
		change({ phase: "downloading" });
		try {
			await update.downloadAndInstall((event) => {
				if (event.event === "Finished") change({ phase: "installing" });
			});
			change({ phase: "installing" });
			await bridge.relaunch();
		} catch (error) {
			change({ phase: "error", error: String(error) });
		}
	}

	return { state, checkForUpdate, installUpdate };
}

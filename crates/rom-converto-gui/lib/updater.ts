import { Channel } from "@tauri-apps/api/core";
import { invoke } from "~/lib/ipc";
import type { UpdateEvent } from "~/types";

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

interface UpdaterBridge {
	/** Resolves to the available version, or null when up to date. */
	check(): Promise<string | null>;
	/** Downloads, installs and restarts; resolves only if the backend returns before exiting. */
	install(onEvent: (event: UpdateEvent) => void): Promise<void>;
}

const ipcBridge: UpdaterBridge = {
	check: () => invoke<string | null>("cmd_update_check"),
	install(onEvent) {
		const channel = new Channel<UpdateEvent>();
		channel.onmessage = onEvent;
		return invoke("cmd_update_install", { onEvent: channel });
	},
};

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
	bridge: UpdaterBridge = ipcBridge,
) {
	const state: UpdateState = { phase: "current", availableVersion: "", progress: -1, error: "" };
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
			const version = await bridge.check();
			change({ phase: version ? "available" : "up-to-date", availableVersion: version ?? "" });
		} catch (error) {
			change({ phase: "error", error: String(error) });
		}
	}

	async function installUpdate() {
		if (state.phase !== "available") return;
		change({ phase: "downloading", progress: -1 });
		try {
			// The backend restarts the app once the install lands.
			await bridge.install((event) => {
				if (event.kind === "progress") change({ progress: Math.min(1, event.downloaded / event.total) });
				else change({ phase: "installing" });
			});
		} catch (error) {
			change({ phase: "error", error: String(error) });
		}
	}

	return { state, checkForUpdate, installUpdate };
}

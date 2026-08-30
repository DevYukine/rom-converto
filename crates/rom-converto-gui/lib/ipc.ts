import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import type { InvokeArgs } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import type { EventCallback, UnlistenFn } from "@tauri-apps/api/event";
import { homeDir } from "@tauri-apps/api/path";
import { open as tauriOpen, save as tauriSave } from "@tauri-apps/plugin-dialog";
import type { OpenDialogOptions, SaveDialogOptions } from "@tauri-apps/plugin-dialog";

// Rewrites `~`-prefixed strings to the user's home directory, recursing into
// plain objects and arrays. The Rust backend doesn't do shell-style tilde
// expansion, so paths must be expanded before crossing the IPC boundary.
export function expandTildePaths(value: unknown, home: string): unknown {
	if (typeof value === "string") {
		if (value === "~") return home;
		if (value.startsWith("~/") || value.startsWith("~\\")) return `${home}${value.slice(1)}`;
		return value;
	}
	if (Array.isArray(value)) return value.map((item) => expandTildePaths(item, home));
	// Only recurse into plain objects; rebuilding a Uint8Array, Map, or File
	// into a plain object would corrupt the IPC payload.
	if (value !== null && typeof value === "object") {
		const proto = Object.getPrototypeOf(value);
		if (proto !== Object.prototype && proto !== null) return value;
		return Object.fromEntries(
			Object.entries(value).map(([key, item]) => [key, expandTildePaths(item, home)]),
		);
	}
	return value;
}

let homeDirPromise: Promise<string> | undefined;

// Single seam between the app and Tauri. In a real window it delegates to the
// Tauri APIs; in a dev browser (no Tauri, `import.meta.dev`) it delegates to a
// mock. The `import.meta.dev` guard makes the dynamic mock import dead code in
// the production bundle, so it is tree-shaken out.
export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function mock() {
	return import("./ipc-mock");
}

export function invoke<T = unknown>(cmd: string, args?: InvokeArgs): Promise<T> {
	if (isTauri) {
		if (!args) return tauriInvoke<T>(cmd, args);
		homeDirPromise ??= homeDir();
		// A failed homeDir() must not poison the cache or block the call;
		// fall back to the unexpanded args and retry the lookup next time.
		return homeDirPromise
			.catch(() => {
				homeDirPromise = undefined;
				return null;
			})
			.then((home) =>
				tauriInvoke<T>(cmd, home ? (expandTildePaths(args, home) as InvokeArgs) : args),
			);
	}
	if (import.meta.dev) return mock().then((m) => m.invoke<T>(cmd, args));
	return Promise.reject(new Error(`ipc unavailable outside Tauri: ${cmd}`));
}

export function listen<T>(event: string, handler: EventCallback<T>): Promise<UnlistenFn> {
	if (isTauri) return tauriListen<T>(event, handler);
	if (import.meta.dev) return mock().then((m) => m.listen<T>(event, handler));
	return Promise.resolve(() => {});
}

export function open(options?: OpenDialogOptions): Promise<string | string[] | null> {
	if (isTauri) return tauriOpen(options) as Promise<string | string[] | null>;
	if (import.meta.dev) return mock().then((m) => m.open(options));
	return Promise.resolve(null);
}

export function save(options?: SaveDialogOptions): Promise<string | null> {
	if (isTauri) return tauriSave(options);
	if (import.meta.dev) return mock().then((m) => m.save(options));
	return Promise.resolve(null);
}

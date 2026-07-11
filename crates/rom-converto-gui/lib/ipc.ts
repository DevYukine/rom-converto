import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import type { InvokeArgs } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import type { EventCallback, UnlistenFn } from "@tauri-apps/api/event";
import { open as tauriOpen, save as tauriSave } from "@tauri-apps/plugin-dialog";
import type { OpenDialogOptions, SaveDialogOptions } from "@tauri-apps/plugin-dialog";

// Single seam between the app and Tauri. In a real window it delegates to the
// Tauri APIs; in a dev browser (no Tauri, `import.meta.dev`) it delegates to a
// mock. The `import.meta.dev` guard makes the dynamic mock import dead code in
// the production bundle, so it is tree-shaken out.
export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function mock() {
	return import("./ipc-mock");
}

export function invoke<T = unknown>(cmd: string, args?: InvokeArgs): Promise<T> {
	if (isTauri) return tauriInvoke<T>(cmd, args);
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

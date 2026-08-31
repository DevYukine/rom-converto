import { reactive } from "vue";
import { basename } from "~/composables/useDerivedPath";
import { invoke } from "../ipc";
import { NX_KEYS_AUTO, type KvColor, type OpStore } from "./types";

// Resolved prod.keys per explicit path ("" = auto), fed by cmd_nx_keys_resolve.
// Missing results are re-probed on later renders so dropping the file into
// ~/.switch turns the row green without restarting the app.
type Status = { resolved: string | null; done: boolean; inflight: boolean; at: number };
const cache = reactive(new Map<string, Status>());
const RECHECK_MS = 5000;

function probe(k: string, keys: string | null): void {
	const cur = cache.get(k);
	if (cur?.inflight) return;
	cache.set(k, { resolved: cur?.resolved ?? null, done: cur?.done ?? false, inflight: true, at: Date.now() });
	invoke<string | null>("cmd_nx_keys_resolve", { keys: keys || null }).then(
		(resolved) => cache.set(k, { resolved, done: true, inflight: false, at: Date.now() }),
		() => cache.set(k, { resolved: null, done: true, inflight: false, at: Date.now() }),
	);
}

function status(keys: string | null): Status | undefined {
	const k = keys || "";
	const s = cache.get(k);
	if (!s || (s.done && !s.resolved && Date.now() - s.at > RECHECK_MS)) {
		// Deferred so the reactive write never happens during component render.
		queueMicrotask(() => probe(k, keys));
	}
	return s;
}

export function nxKeysDisplay(store: OpStore): string {
	const keys = (store.keys as string) || null;
	const s = status(keys);
	if (keys) {
		const name = basename(keys);
		if (!s?.done) return name;
		return s.resolved ? `${name} ✓` : `${name} · not found`;
	}
	if (!s?.done) return NX_KEYS_AUTO;
	return s.resolved ? `auto (${s.resolved}) ✓` : "not found · click to browse";
}

export function nxKeysColor(store: OpStore): KvColor | undefined {
	const s = status((store.keys as string) || null);
	if (!s?.done) return undefined;
	return s.resolved ? "green" : "red";
}

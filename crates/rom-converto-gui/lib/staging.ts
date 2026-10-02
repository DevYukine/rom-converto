import { ref } from "vue";
import { invoke } from "~/lib/ipc";
import { useFolderScan } from "~/composables/useFolderScan";
import { basename } from "~/composables/useDerivedPath";
import type { OpDef, StagedItem } from "~/lib/opdefs/types";

function extOf(path: string): string {
	const name = basename(path);
	const dot = name.lastIndexOf(".");
	return dot === -1 ? "" : name.slice(dot + 1).toLowerCase();
}

async function fileSize(path: string): Promise<number> {
	try {
		return await invoke<number>("cmd_file_size", { path });
	} catch {
		return 0;
	}
}

export function useStaging(def: OpDef) {
	const staged = ref<StagedItem[]>([]);
	const scan = useFolderScan(def.acceptedExts);

	async function add(paths: string[]) {
		const store = def.useStore();
		const recursive = store.recursive !== false;
		const maxDepth = (store.maxDepth as number | null | undefined) ?? null;
		// Ops with a folder picker take the directory itself when it holds no
		// eligible files (NUS/CDN trees, hash). File-input ops must never stage
		// a directory: their commands open the path as a regular file.
		const folderInput = !!(def.browseDirectory || def.browseAlsoDirectory);
		const files: string[] = [];
		const dirs = new Set<string>();
		for (const p of paths) {
			const scanned = await scan.expand(p, recursive ? maxDepth : 1);
			let expanded: string[];
			if (scanned === null) expanded = [p];
			else if (scanned.length > 0) expanded = scanned;
			else if (folderInput) {
				expanded = [p];
				dirs.add(p);
			} else expanded = [];
			for (const f of expanded) if (!files.includes(f)) files.push(f);
		}
		const added: StagedItem[] = [];
		for (const path of files) {
			if (staged.value.some((s) => s.path === path)) continue;
			if (def.singleInput) staged.value = [];
			const item: StagedItem = {
				id: crypto.randomUUID(),
				path,
				name: basename(path),
				size: 0,
				outExt: def.deriveOutput ? extOf(def.deriveOutput(path, store)) : "",
				dir: dirs.has(path),
			};
			staged.value.push(item);
			// `item` is the raw object; only the proxy in `staged.value` is reactive.
			const entry = staged.value[staged.value.length - 1]!;
			added.push(entry);
			void fileSize(path).then((n) => {
				entry.size = n;
			});
		}
		if (!added.length || !def.onStaged) return;
		def.onStaged(store, added);
		// onStaged may switch an option that picks the output extension.
		const derive = def.deriveOutput;
		if (!derive) return;
		const ids = new Set(added.map((item) => item.id));
		for (const item of staged.value) {
			if (ids.has(item.id)) item.outExt = extOf(derive(item.path, store));
		}
	}

	function remove(id: string) {
		staged.value = staged.value.filter((s) => s.id !== id);
	}

	function clear() {
		staged.value = [];
	}

	return { staged, add, remove, clear };
}

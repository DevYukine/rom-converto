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
		for (const p of paths) {
			const scanned = await scan.expand(p, recursive ? maxDepth : 1);
			let expanded: string[];
			if (scanned === null) expanded = [p];
			else if (scanned.length > 0) expanded = scanned;
			else expanded = folderInput ? [p] : [];
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
				outExt: def.deriveOutput ? extOf(def.deriveOutput(path, store)) : extOf(path),
			};
			staged.value.push(item);
			added.push(item);
			void fileSize(path).then((n) => {
				item.size = n;
			});
		}
		if (added.length) def.onStaged?.(store, added);
	}

	function remove(id: string) {
		staged.value = staged.value.filter((s) => s.id !== id);
	}

	function clear() {
		staged.value = [];
	}

	return { staged, add, remove, clear };
}

import { watch } from "vue";
import { useHashStore } from "~/stores/hash";
import { usePlaylistStore } from "~/stores/playlist";
import { useCueMergeStore } from "~/stores/cue-merge";
import { useCtrCdnToCiaStore } from "~/stores/ctr-cdn-to-cia";
import { useCtrGenerateTicketStore } from "~/stores/ctr-generate-ticket";
import { basename, deriveMergedCuePath, withOutputDir } from "~/composables/useDerivedPath";
import { registerOp, type OpDef, type OpStore } from "./types";

function dirName(path: string): string {
	const norm = path.replace(/[\\/]+$/, "");
	const i = Math.max(norm.lastIndexOf("/"), norm.lastIndexOf("\\"));
	return i >= 0 ? norm.slice(0, i) : "";
}

// hash.algos is a string[]; the Options card only renders toggle/kv/slider/etc.
// fields, so each algorithm gets a synthetic boolean accessor backed by the
// same array the store already exposes.
const ALGOS = ["crc32", "sha1", "md5", "sha256"] as const;
type Algo = (typeof ALGOS)[number];

function algoKey(algo: Algo): string {
	return `algo${algo.charAt(0).toUpperCase()}${algo.slice(1)}`;
}

function withAlgoToggles(store: OpStore): OpStore {
	for (const algo of ALGOS) {
		const key = algoKey(algo);
		if (Object.prototype.hasOwnProperty.call(store, key)) continue;
		Object.defineProperty(store, key, {
			configurable: true,
			enumerable: true,
			get: () => (store.algos as string[]).includes(algo),
			set: (v: boolean) => {
				const set = new Set(store.algos as string[]);
				if (v) set.add(algo);
				else set.delete(algo);
				store.algos = ALGOS.filter((a) => set.has(a));
			},
		});
	}
	return store;
}

function useHash(): OpStore {
	return withAlgoToggles(useHashStore());
}

// compress always requires decrypted content, so forcing decrypt on mirrors
// the old page's watcher instead of a silently-wrong combination.
function useCdnToCia(): OpStore {
	const store = useCtrCdnToCiaStore();
	watch(
		() => store.compress,
		(v) => {
			if (v) store.decrypt = true;
		},
	);
	return store;
}

const hash: OpDef = {
	op: "tools",
	console: "hash",
	opLabel: "Tools",
	storeId: "hash",
	useStore: useHash,
	command: "cmd_hash",
	resultKind: "hash",

	title: "Hash files",
	subtitle: "Computes CRC32, SHA-1, MD5 and SHA-256 digests.",
	dropText: "Drop a file or a folder to hash",
	acceptedExts: [],
	browseAlsoDirectory: true,
	progressKey: "hash",

	fields: [
		{ kind: "toggle", key: algoKey("crc32"), label: "CRC32" },
		{ kind: "toggle", key: algoKey("sha1"), label: "SHA-1" },
		{ kind: "toggle", key: algoKey("md5"), label: "MD5" },
		{ kind: "toggle", key: algoKey("sha256"), label: "SHA-256" },
		{ kind: "toggle", key: "recursive", label: "Recursive" },
		{
			kind: "number",
			key: "maxDepth",
			label: "Max depth",
			placeholder: "unlimited",
			visible: (store) => !!store.recursive,
		},
	],
	note: "All digests are computed in one streaming pass per file. Plain checksums only, no database lookup.",
	outputRows: [],

	showConflict: false,
	showDryRun: false,
	actionNote: "Runs in the global queue like everything else.",

	buildArgs: (store, item) => ({
		input: item.path,
		algos: store.algos,
		recursive: store.recursive,
		maxDepth: store.recursive ? store.maxDepth : null,
	}),
	chips: (store) => `${(store.algos as string[]).join("+")}${store.recursive ? " · recursive" : ""}`,
};

const playlist: OpDef = {
	op: "tools",
	console: "playlist",
	opLabel: "Tools",
	storeId: "playlist",
	useStore: () => usePlaylistStore(),
	command: "cmd_playlist",
	resultKind: "text",

	title: "Generate playlists (.m3u)",
	subtitle: "Groups multi-disc sets into .m3u playlists.",
	dropText: "Drop a folder to scan for multi-disc sets",
	acceptedExts: [],
	browseDirectory: true,
	progressKey: "playlist",

	fields: [
		{
			kind: "segmented",
			key: "mode",
			label: "Playlist mode",
			hint: "Multiple writes an .m3u only for sets with 2+ discs. Always covers single-disc games too.",
			options: [
				{ label: "Multiple", value: "multiple" },
				{ label: "Always", value: "always" },
			],
		},
		{ kind: "text", key: "extensions", label: "Extensions", placeholder: "cue,chd,iso,cso,zso" },
		{ kind: "number", key: "maxDepth", label: "Max depth", placeholder: "unlimited" },
	],
	note: "Grouping follows standard disc-set naming tokens, filename-based only. A set mixing formats gets a warning.",
	outputRows: [
		{
			kind: "directory",
			label: "Output directory",
			display: (store) => store.outputDir || "(next to input)",
			set: (store, value) => {
				store.outputDir = value;
			},
		},
	],

	showConflict: true,
	showDryRun: false,
	actionNote: "Runs in the global queue like everything else.",

	buildArgs: (store, item) => ({
		scanDir: item.path,
		outputDir: store.outputDir || null,
		mode: store.mode,
		extensions: store.extensions,
		maxDepth: store.maxDepth,
		onConflict: store.onConflict,
	}),
	chips: (store) => `mode:${store.mode}`,
};

const merge: OpDef = {
	op: "tools",
	console: "merge",
	opLabel: "Tools",
	storeId: "cue-merge",
	useStore: () => useCueMergeStore(),
	command: "cmd_cue_merge",
	resultKind: "text",

	title: "Merge multi-bin",
	subtitle: "Merges a multi-bin .cue into one .bin/.cue pair.",
	dropText: "Drop a multi-bin .cue file",
	acceptedExts: ["cue"],
	browseFilters: [{ name: "CUE", extensions: ["cue"] }],
	progressKey: "cue-merge",

	fields: [],
	note: "Merges a multi-bin .cue into a single .bin/.cue pair for emulators that can't load split images.",
	outputRows: [
		{
			kind: "directory",
			label: "Directory",
			display: (store) => (store.output ? dirName(store.output) : "(next to input)"),
			set: (store, value) => {
				const base = store.output ? basename(store.output) : "merged.cue";
				store.output = withOutputDir(base, value);
			},
		},
		{
			kind: "text",
			label: "File",
			display: (store) => (store.output ? basename(store.output) : "(auto)"),
		},
	],

	showConflict: true,
	showDryRun: true,
	actionNote: "Runs in the global queue like everything else.",

	deriveOutput: (input, store) => store.output || deriveMergedCuePath(input),
	buildArgs: (store, item) => ({
		cuePath: item.path,
		output: store.output || deriveMergedCuePath(item.path),
		onConflict: store.onConflict,
		skipSpaceCheck: store.skipSpaceCheck,
	}),
	chips: (store) => `onConflict:${store.onConflict}`,
};

const cdn2cia: OpDef = {
	op: "tools",
	console: "cdn2cia",
	opLabel: "Tools",
	storeId: "ctr-cdn-to-cia",
	useStore: useCdnToCia,
	command: "cmd_cdn_to_cia",
	resultKind: "text",

	title: "Convert CDN to CIA",
	subtitle: "Builds an installable .cia file from a Nintendo CDN content directory.",
	dropText: "Drop a CDN content directory",
	acceptedExts: [],
	browseDirectory: true,
	progressKey: "cdn-to-cia",

	fields: [
		{ kind: "toggle", key: "decrypt", label: "Decrypt", disabled: (store) => !!store.compress, note: (store) => store.compress && "Forced on: Compress requires decrypted content." },
		{ kind: "toggle", key: "compress", label: "Compress" },
		{ kind: "toggle", key: "ensureTicket", label: "Generate ticket" },
		{ kind: "toggle", key: "recursive", label: "Recursive" },
		{ kind: "toggle", key: "cleanup", label: "Cleanup" },
	],
	outputRows: [
		{
			kind: "directory",
			label: "Directory",
			display: (store) => (store.output ? dirName(store.output) : "(next to input)"),
			set: (store, value) => {
				const base = store.output ? basename(store.output) : "output.cia";
				store.output = withOutputDir(base, value);
			},
		},
		{
			kind: "save",
			label: "File",
			display: (store) => (store.output ? basename(store.output) : "(auto)"),
			set: (store, value) => {
				store.output = value;
			},
			filters: [{ name: "CIA", extensions: ["cia"] }],
		},
	],

	showConflict: true,
	showDryRun: false,
	actionNote: "Runs in the global queue like everything else.",

	deriveOutput: (input, store) => store.output || `${input}.cia`,
	buildArgs: (store, item) => ({
		cdnDir: item.path,
		output: store.output || null,
		decrypt: store.decrypt,
		compress: store.compress,
		cleanup: store.cleanup,
		recursive: store.recursive,
		ensureTicketExists: store.ensureTicket,
		onConflict: store.onConflict,
		skipSpaceCheck: store.skipSpaceCheck,
	}),
	chips: (store) => [store.decrypt && "decrypt", store.compress && "compress", store.ensureTicket && "ticket"]
		.filter(Boolean)
		.join("+"),
};

const ticket: OpDef = {
	op: "tools",
	console: "ticket",
	opLabel: "Tools",
	storeId: "ctr-generate-ticket",
	useStore: () => useCtrGenerateTicketStore(),
	command: "cmd_generate_ticket",
	resultKind: "text",

	title: "Generate ticket",
	subtitle: "Synthesizes a .tik ticket from a CDN content directory's title key and metadata.",
	dropText: "Drop a CDN content directory",
	acceptedExts: [],
	browseDirectory: true,

	fields: [],
	outputRows: [
		{
			kind: "directory",
			label: "Directory",
			display: (store) => (store.output ? dirName(store.output) : "(next to input)"),
			set: (store, value) => {
				const base = store.output ? basename(store.output) : "ticket.tik";
				store.output = withOutputDir(base, value);
			},
		},
		{
			kind: "save",
			label: "File",
			display: (store) => (store.output ? basename(store.output) : "ticket.tik"),
			set: (store, value) => {
				store.output = value;
			},
			filters: [{ name: "Ticket", extensions: ["tik"] }],
			defaultPath: "ticket.tik",
		},
	],

	showConflict: false,
	showDryRun: false,
	actionNote: "Runs in the global queue like everything else.",

	deriveOutput: (input, store) => store.output || `${input}/ticket.tik`,
	buildArgs: (store, item) => ({
		cdnDir: item.path,
		output: store.output || `${item.path}/ticket.tik`,
	}),
	chips: () => "ticket",
};

registerOp("tools", { hash, playlist, merge, cdn2cia, ticket });

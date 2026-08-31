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
		{
			kind: "toggle",
			key: algoKey("crc32"),
			label: "CRC32",
			tooltip: "A fast 32-bit checksum, good for catching corruption or duplicates. Just a digest of the file bytes, not a database lookup.",
		},
		{
			kind: "toggle",
			key: algoKey("sha1"),
			label: "SHA-1",
			tooltip: "A 160-bit hash, the digest most ROM hash sheets use. Just a digest of the file bytes, not a database lookup.",
		},
		{
			kind: "toggle",
			key: algoKey("md5"),
			label: "MD5",
			tooltip: "A 128-bit hash, still common for file verification. Just a digest of the file bytes, not a database lookup.",
		},
		{
			kind: "toggle",
			key: algoKey("sha256"),
			label: "SHA-256",
			tooltip: "A 256-bit hash with no known practical collisions, slower to compute than the others. Just a digest of the file bytes, not a database lookup.",
		},
		{
			kind: "toggle",
			key: "recursive",
			label: "Recursive",
			tooltip: "Hashes every file inside the dropped folder, including subdirectories, instead of only a single file.",
		},
		{
			kind: "number",
			key: "maxDepth",
			label: "Max depth",
			placeholder: "unlimited",
			visible: (store) => !!store.recursive,
			tooltip: "How many folder levels deep the scan goes. Leave blank for unlimited.",
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
		{
			kind: "text",
			key: "extensions",
			label: "Extensions",
			placeholder: "cue,chd,iso,cso,zso",
			tooltip: "Comma-separated list of disc image extensions to scan for, for example cue,chd,iso,cso,zso.",
		},
		{
			kind: "number",
			key: "maxDepth",
			label: "Max depth",
			placeholder: "unlimited",
			tooltip: "How many folder levels deep the scan goes. Leave blank for unlimited.",
		},
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
			tooltip: "Write the generated .m3u files here instead of next to the disc images.",
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
			tooltip: "Directory the merged .cue and .bin pair is written into.",
		},
		{
			kind: "text",
			label: "File",
			display: (store) => (store.output ? basename(store.output) : "(auto)"),
			tooltip: "Filename for the merged .cue file, the .bin is named to match.",
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
		{
			kind: "toggle",
			key: "decrypt",
			label: "Decrypt",
			disabled: (store) => !!store.compress,
			note: (store) => store.compress && "Forced on: Compress requires decrypted content.",
			tooltip: "Decrypt the CIA file after conversion, useful for emulators like Azahar.",
		},
		{
			kind: "toggle",
			key: "compress",
			label: "Compress",
			tooltip: "Compress into Z3DS .zcia format after conversion, which requires the CIA to be decrypted first.",
		},
		{
			kind: "toggle",
			key: "ensureTicket",
			label: "Generate ticket",
			tooltip: "Generates a ticket file in the CDN directory if one is missing. A generated ticket is not official and will not work on a stock 3DS, but works fine on emulators and custom firmware.",
		},
		{
			kind: "toggle",
			key: "recursive",
			label: "Recursive",
			tooltip: "Converts every CDN content subdirectory found, not just the top-level one.",
		},
		{
			kind: "toggle",
			key: "cleanup",
			label: "Cleanup",
			tooltip: "Deletes the original CDN content files after the CIA is created.",
		},
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
			tooltip: "Directory the CIA file is written into.",
		},
		{
			kind: "save",
			label: "File",
			display: (store) => (store.output ? basename(store.output) : "(auto)"),
			set: (store, value) => {
				store.output = value;
			},
			filters: [{ name: "CIA", extensions: ["cia"] }],
			tooltip: "Filename for the output CIA file.",
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
			tooltip: "Directory the ticket file is written into.",
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
			tooltip: "Filename for the generated ticket file.",
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

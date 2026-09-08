import { nxKeysColor, nxKeysDisplay } from "./nx-keys";
import {
	NX_KEYS_TOOLTIP,
	commonOptions,
	directoryOutputRows,
	recursiveFields,
	runArgs,
	templateIsActive,
	type OpDef,
	type OutputRow,
} from "./types";
import { useCtrDecompressStore } from "~/stores/ctr-decompress";
import { useDolDecompressStore } from "~/stores/dol-decompress";
import { useRvlDecompressStore } from "~/stores/rvl-decompress";
import { useNxDecompressStore } from "~/stores/nx-decompress";
import { useChdExtractStore } from "~/stores/chd-extract";
import { useCsoDecompressStore } from "~/stores/cso-decompress";
import { useXboxExtractStore } from "~/stores/xbox-extract";
import { useXenonExtractStore } from "~/stores/xenon-extract";
import { usePspExtractStore } from "~/stores/psp-extract";
import { useVitaExtractStore } from "~/stores/vita-extract";
import {
	basename,
	deriveDecompressedPath,
	deriveDiscPath,
	deriveDiscIsoPath,
	deriveExtractDir,
	deriveNspPath,
	withOutputDir,
} from "~/composables/useDerivedPath";

function outputRows(): OutputRow[] {
	return [
		{
			kind: "directory",
			label: "Directory",
			display: (s) => s.outputDir || "same as source",
			set: (s, v) => { s.outputDir = v; },
			tooltip: "Where extracted files are written. Leave empty to write each output next to its source file.",
		},
		{
			kind: "template",
			label: "Template",
			display: (s) => s.outputTemplate || "",
			set: (s, v) => { s.outputTemplate = v; },
			tooltip:
				"Optional filename pattern built from tokens like {title}, {titleId}, {region}, {console}, {serial}, {ext}, and {basename}. Values come from the file's extracted metadata; a token that can't be resolved falls back to the input's plain filename. Combined with the output directory above.",
		},
	];
}

function outputRowsWithReport(): OutputRow[] {
	return [
		...outputRows(),
		{
			kind: "report",
			label: "Run report",
			display: (s) => (s.reportFile ? basename(s.reportFile) : "none"),
			set: (s, v) => { s.reportFile = v; },
			tooltip:
				"Saves a summary of the run to this file when set. The format is chosen from the file extension (csv, json, html, or htm); any other extension defaults to json.",
		},
	];
}

const EXTRACT_DIR_TOOLTIP =
	"Where the extracted files are written. Leave empty to create a folder next to the input file.";

const ARCHIVE_EXTS = ["zip", "7z", "rar", "tar", "tgz", "gz"];

const ctr: OpDef = {
	op: "extract",
	console: "ctr",
	opLabel: "Extract",
	storeId: "ctr-decompress",
	useStore: useCtrDecompressStore,
	command: "cmd_run",
	resultKind: "convert",
	title: "Decompress / Extract",
	subtitle: "Restores the raw image from a compressed container.",
	dropText: "Drop compressed files or folders",
	acceptedExts: ["zcia", "zcci", "zcxi", "z3dsx", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "Compressed 3DS", extensions: ["zcia", "zcci", "zcxi", "z3dsx"] }],
	fields: [
		{
			kind: "kv",
			key: "accepts",
			label: "Accepts",
			display: () => ".zcia .zcci .zcxi .z3dsx",
			tooltip: "Restores a Z3DS compressed CIA, CCI, CXI, or 3DSX file to its original format.",
		},
		...recursiveFields(),
	],
	note: "Restores the original ROM byte-identically.",
	outputRows: outputRows(),
	actionNote: "Extraction never overwrites the compressed source.",
	deriveOutput: deriveDecompressedPath,
	buildArgs: (store, item, taskId) =>
		runArgs(
			"ctr.decompress",
			item.path,
			templateIsActive(store) ? null : withOutputDir(deriveDecompressedPath(item.path), store.outputDir || ""),
			commonOptions(store),
			false,
			taskId,
		),
	chips: () => "",
};

const dol: OpDef = {
	op: "extract",
	console: "dol",
	opLabel: "Extract",
	storeId: "dol-decompress",
	useStore: useDolDecompressStore,
	command: "cmd_run",
	resultKind: "convert",
	title: "Decompress / Extract",
	subtitle: "Restores the raw image from a compressed container.",
	dropText: "Drop compressed files or folders",
	acceptedExts: ["rvz", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "RVZ", extensions: ["rvz"] }],
	progressKey: "dol-decompress",
	fields: recursiveFields(),
	note: "Output is byte-identical to Dolphin's own decoder.",
	outputRows: outputRows(),
	actionNote: "Extraction never overwrites the compressed source.",
	deriveOutput: deriveDiscIsoPath,
	buildArgs: (store, item, taskId) =>
		runArgs(
			"dol.decompress",
			item.path,
			templateIsActive(store) ? null : withOutputDir(deriveDiscIsoPath(item.path), store.outputDir || ""),
			commonOptions(store),
			false,
			taskId,
			store.reportFile || null,
		),
	chips: () => "",
};

const rvl: OpDef = {
	op: "extract",
	console: "rvl",
	opLabel: "Extract",
	storeId: "rvl-decompress",
	useStore: useRvlDecompressStore,
	command: "cmd_run",
	resultKind: "convert",
	title: "Decompress / Extract",
	subtitle: "Restores the raw image from a compressed container.",
	dropText: "Drop compressed files or folders",
	acceptedExts: ["rvz", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "RVZ", extensions: ["rvz"] }],
	progressKey: "rvl-decompress",
	fields: [
		{
			kind: "segmented",
			key: "format",
			label: "Output format",
			options: [
				{ label: "ISO", value: "iso" },
				{ label: "WBFS", value: "wbfs" },
			],
			tooltip: "WBFS is the format Wii disc loaders and USB drives expect; ISO is a plain, unwrapped disc image.",
		},
		...recursiveFields(),
	],
	note: "Output is byte-identical to Dolphin's own decoder.",
	outputRows: outputRows(),
	actionNote: "Extraction never overwrites the compressed source.",
	deriveOutput: (input, store) => deriveDiscPath(input, store.format),
	buildArgs: (store, item, taskId) =>
		runArgs(
			"rvl.decompress",
			item.path,
			templateIsActive(store) ? null : withOutputDir(deriveDiscPath(item.path, store.format), store.outputDir || ""),
			commonOptions(store),
			false,
			taskId,
			store.reportFile || null,
		),
	chips: (store) => (store.format === "wbfs" ? "wbfs" : ""),
};

const nx: OpDef = {
	op: "extract",
	console: "nx",
	opLabel: "Extract",
	storeId: "nx-decompress",
	useStore: useNxDecompressStore,
	command: "cmd_run",
	resultKind: "convert",
	title: "Decompress / Extract",
	subtitle: "Restores the raw image from a compressed container.",
	dropText: "Drop compressed files or folders",
	acceptedExts: ["nsz", "xcz", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "NSZ/XCZ", extensions: ["nsz", "xcz"] }],
	fields: [
		{
			kind: "file",
			key: "keys",
			label: "prod.keys",
			tooltip: NX_KEYS_TOOLTIP,
			filters: [{ name: "prod.keys", extensions: ["keys", "txt"] }],
			display: nxKeysDisplay,
			color: nxKeysColor,
		},
		...recursiveFields(),
	],
	note: "Output is byte-identical to the original installable NSP / XCI.",
	outputRows: outputRows(),
	actionNote: "Extraction never overwrites the compressed source.",
	deriveOutput: deriveNspPath,
	buildArgs: (store, item, taskId) =>
		runArgs(
			"nx.decompress",
			item.path,
			templateIsActive(store) ? null : withOutputDir(deriveNspPath(item.path), store.outputDir || ""),
			{ keys: store.keys || null, ...commonOptions(store) },
			false,
			taskId,
			store.reportFile || null,
		),
	chips: (store) => (store.keys ? "keys" : ""),
};

const chd: OpDef = {
	op: "extract",
	console: "chd",
	opLabel: "Extract",
	storeId: "chd-extract",
	useStore: useChdExtractStore,
	command: "cmd_run",
	resultKind: "convert",
	title: "Decompress / Extract",
	subtitle: "Restores the raw image from a compressed container.",
	dropText: "Drop compressed files or folders",
	acceptedExts: ["chd", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "CHD", extensions: ["chd"] }],
	fields: [
		{
			kind: "file",
			key: "parent",
			label: "Parent CHD",
			filters: [{ name: "CHD", extensions: ["chd"] }],
			display: (s) => (s.parent ? basename(s.parent) : "none"),
			tooltip:
				"Some CHDs are delta files that only store the differences against a base image. Pick that base CHD here so the full data can be rebuilt.",
		},
		...recursiveFields(),
	],
	note: "CD-mode CHDs extract to .bin + .cue, DVD-mode (PS2/PSP) to a single .iso. The mode is read from the file.",
	outputRows: outputRowsWithReport(),
	showConflict: false,
	actionNote: "Extraction never overwrites the compressed source.",
	deriveOutput: deriveDiscIsoPath,
	buildArgs: (store, item, taskId) =>
		runArgs(
			"chd.extract",
			item.path,
			templateIsActive(store) ? null : withOutputDir(deriveDiscIsoPath(item.path), store.outputDir || ""),
			{ parent: store.parent || null, ...commonOptions(store) },
			false,
			taskId,
			store.reportFile || null,
		),
	chips: (store) => (store.parent ? "parent" : ""),
};

const cso: OpDef = {
	op: "extract",
	console: "cso",
	opLabel: "Extract",
	storeId: "cso-decompress",
	useStore: useCsoDecompressStore,
	command: "cmd_run",
	resultKind: "convert",
	title: "Decompress / Extract",
	subtitle: "Restores the raw image from a compressed container.",
	dropText: "Drop compressed files or folders",
	acceptedExts: ["cso", "zso", "dax", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "CSO/ZSO/DAX", extensions: ["cso", "zso", "dax"] }],
	fields: [
		{
			kind: "kv",
			key: "accepts",
			label: "Accepts",
			display: () => ".cso .zso .dax",
			tooltip:
				"Restores a CSO, ZSO, or DAX compressed disc image to a plain ISO. The container type is detected by its contents, not its extension.",
		},
		...recursiveFields(),
	],
	note: "Container detected by magic, not extension. DAX (PSP legacy) is decode-only.",
	outputRows: outputRowsWithReport(),
	actionNote: "Extraction never overwrites the compressed source.",
	deriveOutput: deriveDiscIsoPath,
	buildArgs: (store, item, taskId) =>
		runArgs(
			"cso.decompress",
			item.path,
			templateIsActive(store) ? null : withOutputDir(deriveDiscIsoPath(item.path), store.outputDir || ""),
			commonOptions(store),
			false,
			taskId,
			store.reportFile || null,
		),
	chips: () => "",
};

const xbox: OpDef = {
	op: "extract",
	console: "xbox",
	opLabel: "Extract",
	storeId: "xbox-extract",
	useStore: useXboxExtractStore,
	command: "cmd_run",
	resultKind: "convert",
	title: "Extract XISO",
	subtitle: "Walks the disc's file tree and writes every file to a folder.",
	dropText: "Drop an .xiso or .iso file",
	acceptedExts: ["xiso", "iso", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "XISO", extensions: ["xiso", "iso"] }],
	fields: recursiveFields(),
	outputRows: directoryOutputRows(EXTRACT_DIR_TOOLTIP),
	actionNote: "Extraction never overwrites the source image.",
	deriveOutput: deriveExtractDir,
	buildArgs: (store, item, taskId) =>
		runArgs(
			"xbox.extract",
			item.path,
			withOutputDir(deriveExtractDir(item.path), store.outputDir || ""),
			{ on_conflict: store.onConflict, skip_space_check: store.skipSpaceCheck },
			false,
			taskId,
		),
	chips: () => "",
};

const xenon: OpDef = {
	op: "extract",
	console: "xenon",
	opLabel: "Extract",
	storeId: "xenon-extract",
	useStore: useXenonExtractStore,
	command: "cmd_run",
	resultKind: "convert",
	title: "Extract ZArchive",
	subtitle: "Writes every file in the archive to a folder.",
	dropText: "Drop a .zar file",
	acceptedExts: ["zar", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "ZArchive", extensions: ["zar"] }],
	fields: recursiveFields(),
	outputRows: directoryOutputRows(EXTRACT_DIR_TOOLTIP),
	actionNote: "Extraction never overwrites the source archive.",
	deriveOutput: deriveExtractDir,
	buildArgs: (store, item, taskId) =>
		runArgs(
			"xenon.extract",
			item.path,
			withOutputDir(deriveExtractDir(item.path), store.outputDir || ""),
			{ on_conflict: store.onConflict, skip_space_check: store.skipSpaceCheck },
			false,
			taskId,
		),
	chips: () => "",
};

const psp: OpDef = {
	op: "extract",
	console: "psp",
	opLabel: "Extract",
	storeId: "psp-extract",
	useStore: usePspExtractStore,
	command: "cmd_run",
	resultKind: "convert",
	title: "Extract EBOOT.PBP",
	subtitle: "Writes every segment (SFO, icons, DATA.PSAR, ...) to a folder.",
	dropText: "Drop an EBOOT.PBP file",
	acceptedExts: ["pbp", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "PBP", extensions: ["pbp"] }],
	fields: recursiveFields(),
	note: "DATA.PSAR is written as stored: still encrypted for PSN (NPUMDIMG) images.",
	outputRows: directoryOutputRows(EXTRACT_DIR_TOOLTIP),
	actionNote: "Extraction never overwrites the source file.",
	deriveOutput: deriveExtractDir,
	buildArgs: (store, item, taskId) =>
		runArgs(
			"psp.extract",
			item.path,
			withOutputDir(deriveExtractDir(item.path), store.outputDir || ""),
			{ on_conflict: store.onConflict, skip_space_check: store.skipSpaceCheck },
			false,
			taskId,
		),
	chips: () => "",
};

const vita: OpDef = {
	op: "extract",
	console: "vita",
	opLabel: "Extract",
	storeId: "vita-extract",
	useStore: useVitaExtractStore,
	command: "cmd_run",
	resultKind: "convert",
	title: "Extract PKG",
	subtitle: "Decrypts the item table and writes every file to a folder.",
	dropText: "Drop a .pkg file",
	acceptedExts: ["pkg", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "PKG", extensions: ["pkg"] }],
	fields: recursiveFields(),
	outputRows: directoryOutputRows(EXTRACT_DIR_TOOLTIP),
	actionNote: "Extraction never overwrites the source file.",
	deriveOutput: deriveExtractDir,
	buildArgs: (store, item, taskId) =>
		runArgs(
			"vita.extract",
			item.path,
			withOutputDir(deriveExtractDir(item.path), store.outputDir || ""),
			{ on_conflict: store.onConflict, skip_space_check: store.skipSpaceCheck },
			false,
			taskId,
		),
	chips: () => "",
};

export const extractOps: OpDef[] = [ctr, dol, rvl, nx, chd, cso, xbox, xenon, psp, vita];

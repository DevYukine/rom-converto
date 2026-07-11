import { recursiveFields, registerOp, templateIsActive, type OpDef, type OutputRow } from "./types";
import { useCtrDecompressStore } from "~/stores/ctr-decompress";
import { useDolDecompressStore } from "~/stores/dol-decompress";
import { useRvlDecompressStore } from "~/stores/rvl-decompress";
import { useNxDecompressStore } from "~/stores/nx-decompress";
import { useChdExtractStore } from "~/stores/chd-extract";
import { useCsoDecompressStore } from "~/stores/cso-decompress";
import {
	basename,
	deriveDecompressedPath,
	deriveDiscPath,
	deriveDiscIsoPath,
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
		},
		{
			kind: "template",
			label: "Template",
			display: (s) => s.outputTemplate || "",
			set: (s, v) => { s.outputTemplate = v; },
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
		},
	];
}

const ARCHIVE_EXTS = ["zip", "7z", "rar", "tar", "tgz", "gz"];

const ctr: OpDef = {
	op: "extract",
	console: "ctr",
	opLabel: "Extract",
	storeId: "ctr-decompress",
	useStore: useCtrDecompressStore,
	command: "cmd_decompress_rom",
	resultKind: "convert",
	title: "Decompress / Extract",
	subtitle: "Restores the raw image from a compressed container.",
	dropText: "Drop compressed files or folders",
	acceptedExts: ["zcia", "zcci", "zcxi", "z3dsx", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "Compressed 3DS", extensions: ["zcia", "zcci", "zcxi", "z3dsx"] }],
	fields: [
		{ kind: "kv", key: "accepts", label: "Accepts", display: () => ".zcia .zcci .zcxi .z3dsx" },
		...recursiveFields(),
	],
	note: "Restores the original ROM byte-identically.",
	outputRows: outputRows(),
	showVerify: true,
	verifyLabel: "Verify after extraction",
	actionNote: "Extraction never overwrites the compressed source.",
	deriveOutput: deriveDecompressedPath,
	buildArgs: (store, item, taskId) => {
		const tmpl = templateIsActive(store);
		return {
			input: item.path,
			output: tmpl ? null : withOutputDir(deriveDecompressedPath(item.path), store.outputDir || ""),
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			dryRun: false,
			taskId,
		};
	},
	chips: () => "",
};

const dol: OpDef = {
	op: "extract",
	console: "dol",
	opLabel: "Extract",
	storeId: "dol-decompress",
	useStore: useDolDecompressStore,
	command: "cmd_decompress_disc",
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
	showVerify: true,
	verifyLabel: "Verify after extraction",
	actionNote: "Extraction never overwrites the compressed source.",
	deriveOutput: deriveDiscIsoPath,
	buildArgs: (store, item, taskId) => {
		const tmpl = templateIsActive(store);
		return {
			input: item.path,
			output: tmpl ? null : withOutputDir(deriveDiscIsoPath(item.path), store.outputDir || ""),
			taskId,
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			report: !!store.reportFile,
			reportFile: store.reportFile || null,
			dryRun: false,
		};
	},
	chips: () => "",
};

const rvl: OpDef = {
	op: "extract",
	console: "rvl",
	opLabel: "Extract",
	storeId: "rvl-decompress",
	useStore: useRvlDecompressStore,
	command: "cmd_decompress_disc",
	resultKind: "convert",
	title: "Decompress / Extract",
	subtitle: "Restores the raw image from a compressed container.",
	dropText: "Drop compressed files or folders",
	acceptedExts: ["rvz", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "RVZ", extensions: ["rvz"] }],
	progressKey: "rvl-decompress",
	fields: [
		{ kind: "segmented", key: "format", label: "Output format", options: [
			{ label: "ISO", value: "iso" },
			{ label: "WBFS", value: "wbfs" },
		] },
		...recursiveFields(),
	],
	note: "Output is byte-identical to Dolphin's own decoder.",
	outputRows: outputRows(),
	showVerify: true,
	verifyLabel: "Verify after extraction",
	actionNote: "Extraction never overwrites the compressed source.",
	deriveOutput: (input, store) => deriveDiscPath(input, store.format),
	buildArgs: (store, item, taskId) => {
		const tmpl = templateIsActive(store);
		return {
			input: item.path,
			output: tmpl ? null : withOutputDir(deriveDiscPath(item.path, store.format), store.outputDir || ""),
			taskId,
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			report: !!store.reportFile,
			reportFile: store.reportFile || null,
			dryRun: false,
		};
	},
	chips: (store) => (store.format === "wbfs" ? "wbfs" : ""),
};

const nx: OpDef = {
	op: "extract",
	console: "nx",
	opLabel: "Extract",
	storeId: "nx-decompress",
	useStore: useNxDecompressStore,
	command: "cmd_nx_decompress",
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
			filters: [{ name: "prod.keys", extensions: ["keys", "txt"] }],
			display: (store) => store.keys || "none",
		},
		...recursiveFields(),
	],
	note: "Output is byte-identical to the original installable NSP / XCI.",
	outputRows: outputRows(),
	showVerify: true,
	verifyLabel: "Verify after extraction",
	actionNote: "Extraction never overwrites the compressed source.",
	deriveOutput: deriveNspPath,
	buildArgs: (store, item, taskId) => {
		const tmpl = templateIsActive(store);
		return {
			input: item.path,
			output: tmpl ? null : withOutputDir(deriveNspPath(item.path), store.outputDir || ""),
			keys: store.keys || null,
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			report: !!store.reportFile,
			reportFile: store.reportFile || null,
			dryRun: false,
			taskId,
		};
	},
	chips: (store) => (store.keys ? "keys" : ""),
};

const chd: OpDef = {
	op: "extract",
	console: "chd",
	opLabel: "Extract",
	storeId: "chd-extract",
	useStore: useChdExtractStore,
	command: "cmd_chd_extract",
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
			tooltip: "-p / --parent for parent-child CHDs",
		},
		...recursiveFields(),
	],
	note: "CD-mode CHDs extract to .bin + .cue, DVD-mode (PS2/PSP) to a single .iso. The mode is read from the file.",
	outputRows: outputRowsWithReport(),
	showConflict: false,
	showVerify: true,
	verifyLabel: "Verify after extraction",
	actionNote: "Extraction never overwrites the compressed source.",
	deriveOutput: deriveDiscIsoPath,
	buildArgs: (store, item, taskId) => {
		const tmpl = templateIsActive(store);
		return {
			input: item.path,
			output: tmpl ? null : withOutputDir(deriveDiscIsoPath(item.path), store.outputDir || ""),
			parent: store.parent || null,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			report: !!store.reportFile,
			reportFile: store.reportFile || null,
			dryRun: false,
			taskId,
		};
	},
	chips: (store) => (store.parent ? "parent" : ""),
};

const cso: OpDef = {
	op: "extract",
	console: "cso",
	opLabel: "Extract",
	storeId: "cso-decompress",
	useStore: useCsoDecompressStore,
	command: "cmd_cso_decompress",
	resultKind: "convert",
	title: "Decompress / Extract",
	subtitle: "Restores the raw image from a compressed container.",
	dropText: "Drop compressed files or folders",
	acceptedExts: ["cso", "zso", "dax", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "CSO/ZSO/DAX", extensions: ["cso", "zso", "dax"] }],
	fields: [
		{ kind: "kv", key: "accepts", label: "Accepts", display: () => ".cso .zso .dax" },
		...recursiveFields(),
	],
	note: "Container detected by magic, not extension. DAX (PSP legacy) is decode-only.",
	outputRows: outputRowsWithReport(),
	showVerify: true,
	verifyLabel: "Verify after extraction",
	actionNote: "Extraction never overwrites the compressed source.",
	deriveOutput: deriveDiscIsoPath,
	buildArgs: (store, item, taskId) => {
		const tmpl = templateIsActive(store);
		return {
			inputPath: item.path,
			output: tmpl ? null : withOutputDir(deriveDiscIsoPath(item.path), store.outputDir || ""),
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			report: !!store.reportFile,
			reportFile: store.reportFile || null,
			dryRun: false,
			taskId,
		};
	},
	chips: () => "",
};

registerOp("extract", { ctr, dol, rvl, nx, chd, cso });

import { recursiveFields, registerOp, templateIsActive, type OpDef } from "./types";
import {
	CHD_CODEC_OPTIONS,
	CHD_CODEC_PLACEHOLDER,
	CHD_DVD_CODEC_OPTIONS,
	CHD_DVD_ZSTD_HINT,
	CHD_LEVEL_HINT,
} from "./compress";
import { useCtrConvertStore } from "~/stores/ctr-convert";
import { useCsoToChdStore } from "~/stores/cso-to-chd";
import { useChdToCsoStore } from "~/stores/chd-to-cso";
import { useCueConvertStore } from "~/stores/cue-convert";
import { useXboxConvertStore } from "~/stores/xbox-convert";
import { basename, deriveConvertedPath, deriveChdPath, deriveCsoPath, deriveDiscIsoPath, deriveXisoPath, withOutputDir } from "~/composables/useDerivedPath";

const ARCHIVE_EXTS = ["zip", "7z", "rar", "tar", "tgz", "gz"];

function templateOutputRows(): OpDef["outputRows"] {
	return [
		{
			kind: "directory",
			label: "Directory",
			display: (s) => s.outputDir || "same as source",
			set: (s, v) => { s.outputDir = v; },
			tooltip: "Where converted files are written. Leave empty to write each output next to its source file.",
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

function templateOutputRowsWithReport(): OpDef["outputRows"] {
	return [
		...templateOutputRows(),
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

const ctr: OpDef = {
	op: "convert",
	console: "ctr",
	opLabel: "Convert",
	storeId: "ctr-convert",
	useStore: useCtrConvertStore,
	command: "cmd_convert_ctr",
	resultKind: "convert",
	title: "Convert CIA ↔ CCI",
	subtitle: "Converts between the installable CIA container and the raw CCI cart image.",
	dropText: "Drop .cia or .cci/.3ds files or folders",
	acceptedExts: ["cia", "3ds", "cci", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "3DS", extensions: ["cia", "3ds", "cci"] }],
	fields: [
		{
			kind: "kv",
			key: "direction",
			label: "Direction",
			display: () => "auto (CIA ↔ CCI)",
			tooltip:
				"Detected from the input file: a CIA converts to a 3DS/CCI cart image, while a 3DS or CCI file converts to a CIA.",
		},
		...recursiveFields(),
	],
	note: "Produces an unsigned CIA with a zero title key: works on CFW and emulators, not installable on stock hardware.",
	outputRows: templateOutputRows(),
	showVerify: true,
	verifyLabel: "Compute output hash",
	actionNote: "Jobs start automatically. Parameters lock once queued.",
	deriveOutput: deriveConvertedPath,
	buildArgs: (store, item, taskId) => {
		const tmpl = templateIsActive(store);
		return {
			input: item.path,
			output: tmpl ? null : withOutputDir(deriveConvertedPath(item.path), store.outputDir || ""),
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			verifyAfter: store.verifyAfter,
			dryRun: false,
			taskId,
		};
	},
	chips: () => "",
};

const cso: OpDef = {
	op: "convert",
	console: "cso",
	opLabel: "Convert",
	storeId: "cso-to-chd",
	useStore: useCsoToChdStore,
	command: "cmd_cso_to_chd",
	resultKind: "convert",
	title: "Convert ISO → CHD",
	subtitle: "Decodes a CSO/ZSO/DAX disc image and rebuilds it as a CHD.",
	dropText: "Drop .cso, .zso or .dax files or folders",
	acceptedExts: ["cso", "zso", "dax", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "CSO/ZSO/DAX", extensions: ["cso", "zso", "dax"] }],
	fields: [
		{
			kind: "segmented",
			key: "mode",
			label: "Disc mode",
			options: [
				{ label: "Auto", value: "auto" },
				{ label: "CD", value: "cd" },
				{ label: "DVD", value: "dvd" },
			],
			tooltip:
				"How the CHD is structured: CD mode for PS1 and PS2-CD games, DVD mode for PS2-DVD and PSP games. Auto detects this from the disc image.",
		},
		{
			kind: "number",
			key: "hunkSize",
			label: "Hunk size",
			placeholder: "auto",
			tooltip:
				"Size of each compressed block in bytes, a multiple of 2048. Automatically uses 4096, or 2048 for detected PSP images since PPSSPP reads 2048 byte blocks.",
		},
		{
			kind: "multiselect",
			key: "codecs",
			label: "Codecs",
			options: CHD_CODEC_OPTIONS,
			max: 4,
			placeholder: CHD_CODEC_PLACEHOLDER,
			visible: (s) => s.mode !== "dvd",
			tooltip:
				"Compressors tried in order for each block, up to 4. CD deflate (cdzl), CD zstd (cdzs), CD lzma (cdlz), and CD flac (cdfl, for audio tracks) are built for CD-mode discs; deflate (zlib), zstd, lzma, huffman (huff), and flac work generically. Leave empty for the automatic CD-mode set (cdlz, cdzl, cdfl).",
		},
		{
			kind: "multiselect",
			key: "codecs",
			label: "Codecs",
			options: CHD_DVD_CODEC_OPTIONS,
			max: 4,
			placeholder: CHD_CODEC_PLACEHOLDER,
			hint: CHD_DVD_ZSTD_HINT,
			visible: (s) => s.mode === "dvd",
			tooltip:
				"Compressors tried in order for each block, up to 4: deflate (zlib), zstd, lzma, huffman (huff), and flac. Leave empty for the automatic DVD-mode set (lzma, zlib, huff, flac).",
		},
		{
			kind: "number",
			key: "level",
			label: "Level",
			placeholder: "auto",
			hint: CHD_LEVEL_HINT,
			tooltip:
				"Compression strength from 1 to 22. Zstandard uses the value directly; deflate and lzma cap at 9. Leave empty to use each codec's default (zstd 19, lzma 8, zlib 9).",
		},
		...recursiveFields(),
	],
	note: "Decodes to a temporary ISO next to the output, builds the CHD, and always removes the temp ISO on success, failure or cancel.",
	outputRows: templateOutputRowsWithReport(),
	showVerify: true,
	verifyLabel: "Verify after conversion",
	actionNote: "Jobs start automatically. Parameters lock once queued.",
	deriveOutput: deriveChdPath,
	buildArgs: (store, item, taskId) => {
		const tmpl = templateIsActive(store);
		return {
			inputPath: item.path,
			output: tmpl ? null : withOutputDir(deriveChdPath(item.path), store.outputDir || ""),
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			codecs: store.codecs.length ? store.codecs : null,
			level: store.level,
			mode: store.mode === "auto" ? null : store.mode,
			hunkSize: store.hunkSize || null,
			outputTemplate: store.outputTemplate || null,
			report: !!store.reportFile,
			reportFile: store.reportFile || null,
			verifyAfter: store.verifyAfter,
			dryRun: false,
			taskId,
		};
	},
	chips: (store) =>
		[
			store.mode !== "auto" ? store.mode : "",
			store.codecs.length ? store.codecs.join(", ") : "",
			store.level ? `level ${store.level}` : "",
		]
			.filter(Boolean)
			.join(" · "),
};

const chd: OpDef = {
	op: "convert",
	console: "chd",
	opLabel: "Convert",
	storeId: "chd-to-cso",
	useStore: useChdToCsoStore,
	command: "cmd_chd_to_cso",
	resultKind: "convert",
	title: "Convert CHD → CSO/ZSO",
	subtitle: "Decodes a DVD-mode CHD and re-encodes it as CSO or ZSO.",
	dropText: "Drop .chd files or folders",
	acceptedExts: ["chd", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "CHD", extensions: ["chd"] }],
	fields: [
		{
			kind: "segmented",
			key: "format",
			label: "Format",
			options: [
				{ label: "CSO", value: "cso" },
				{ label: "ZSO", value: "zso" },
			],
			tooltip:
				"CSO (CISO v1, deflate) targets PSP hardware with custom firmware and PPSSPP. ZSO (ZISO, LZ4) targets Open PS2 Loader on real PS2 hardware and ARK-4 on PSP.",
		},
		{
			kind: "number",
			key: "blockSize",
			label: "Block size",
			placeholder: "default",
			tooltip:
				"Block size in bytes, a power of two. Defaults to 2048, or 16384 for inputs 2 GiB and larger, matching maxcso.",
		},
		...recursiveFields(),
	],
	note: "DVD-mode CHDs only: a CD-mode CHD has no flat ISO for CSO/ZSO to hold, and is rejected up front.",
	outputRows: templateOutputRowsWithReport(),
	showVerify: true,
	verifyLabel: "Verify after conversion",
	actionNote: "Jobs start automatically. Parameters lock once queued.",
	deriveOutput: (input, store) => deriveCsoPath(input, store.format),
	buildArgs: (store, item, taskId) => {
		const tmpl = templateIsActive(store);
		return {
			inputPath: item.path,
			output: tmpl ? null : withOutputDir(deriveCsoPath(item.path, store.format), store.outputDir || ""),
			format: store.format,
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			blockSize: store.blockSize || null,
			outputTemplate: store.outputTemplate || null,
			report: !!store.reportFile,
			reportFile: store.reportFile || null,
			verifyAfter: store.verifyAfter,
			dryRun: false,
			taskId,
		};
	},
	chips: (store) => store.format,
};

// ISO goes through cmd_cue_to_iso (no format arg); CSO and ZSO share
// cmd_cue_to_cso, which takes the container format as an argument.
function deriveCueOutput(input: string, format: string): string {
	return format === "iso" ? deriveDiscIsoPath(input) : deriveCsoPath(input, format as "cso" | "zso");
}

const cue: OpDef = {
	op: "convert",
	console: "cue",
	opLabel: "Convert",
	storeId: "cue-convert",
	useStore: useCueConvertStore,
	command: (store) => (store.format === "iso" ? "cmd_cue_to_iso" : "cmd_cue_to_cso"),
	resultKind: "convert",
	title: "Convert CUE/BIN",
	subtitle: "Converts a CUE/BIN disc image's data track to ISO, or to a block-compressed CSO or ZSO.",
	dropText: "Drop a .cue file or folder",
	acceptedExts: ["cue", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "CUE", extensions: ["cue"] }],
	singleInput: true,
	progressKey: (s) => (s.format === "iso" ? "cue-to-iso" : "cue-to-cso"),
	fields: [
		{
			kind: "segmented",
			key: "format",
			label: "Format",
			options: [
				{ label: "ISO", value: "iso" },
				{ label: "CSO", value: "cso" },
				{ label: "ZSO", value: "zso" },
			],
			// A hand-picked output file carries the old format's extension; drop it.
			onSet: (s) => {
				s.output = "";
			},
			tooltip:
				"ISO extracts the plain data track. CSO (CISO v1, deflate) targets PSP hardware with custom firmware and PPSSPP. ZSO (ZISO, LZ4) targets Open PS2 Loader on real PS2 hardware and ARK-4 on PSP. Audio tracks are always skipped.",
		},
	],
	note: "Audio tracks are skipped; only the data track is converted.",
	outputRows: [
		{
			kind: "directory",
			label: "Directory",
			display: (s) => s.outputDir || "same as source",
			set: (s, v) => { s.outputDir = v; },
			tooltip: "Where the converted file is written. Leave empty to write it next to its source file.",
		},
		{
			kind: "save",
			label: "File",
			display: (s) => (s.output ? basename(s.output) : "(auto)"),
			set: (s, v) => { s.output = v; },
			filters: [{ name: "Image", extensions: ["iso", "cso", "zso"] }],
			tooltip: "Pick an exact output file path. Leave unset to derive the name from the input and chosen format.",
		},
	],
	actionNote: "Jobs start automatically. Parameters lock once queued.",
	deriveOutput: (input, store) => deriveCueOutput(input, store.format),
	buildArgs: (store, item) => {
		const output = store.output || withOutputDir(deriveCueOutput(item.path, store.format), store.outputDir || "");
		if (store.format === "iso") {
			return {
				cuePath: item.path,
				output,
				onConflict: store.onConflict,
				skipSpaceCheck: store.skipSpaceCheck,
				dryRun: false,
			};
		}
		return {
			cuePath: item.path,
			output,
			format: store.format,
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			dryRun: false,
		};
	},
	chips: (store) => store.format,
};

const xbox: OpDef = {
	op: "convert",
	console: "xbox",
	opLabel: "Convert",
	storeId: "xbox-convert",
	useStore: useXboxConvertStore,
	command: "cmd_xbox_convert",
	resultKind: "convert",
	title: "Convert ISO → XISO",
	subtitle: "Trims a full disc image down to the game partition, or packs a directory of extracted files.",
	dropText: "Drop a full disc image (.iso), or a folder of extracted game files",
	acceptedExts: ["iso", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "Xbox", extensions: ["iso"] }],
	browseAlsoDirectory: true,
	fields: [
		{
			kind: "toggle",
			key: "mediaPatch",
			label: "XBE media patch",
			tooltip:
				"Patches the XDK media-type check in every .xbe so the image boots on BIOSes that need it. Inert on files that do not, so leaving it on is safe.",
		},
		...recursiveFields(),
	],
	note: "The media patch is inert on .xbe files that don't need it, so leaving it on is safe by default.",
	outputRows: templateOutputRowsWithReport(),
	showVerify: true,
	verifyLabel: "Verify after conversion",
	actionNote: "Jobs start automatically. Parameters lock once queued.",
	deriveOutput: deriveXisoPath,
	buildArgs: (store, item, taskId) => {
		const tmpl = templateIsActive(store);
		return {
			input: item.path,
			output: tmpl ? null : withOutputDir(deriveXisoPath(item.path), store.outputDir || ""),
			mediaPatch: store.mediaPatch,
			taskId,
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			report: !!store.reportFile,
			reportFile: store.reportFile || null,
			verifyAfter: store.verifyAfter,
			dryRun: false,
		};
	},
	chips: (store) => (store.mediaPatch ? "" : "no media patch"),
};

registerOp("convert", { ctr, cso, chd, cue, xbox });

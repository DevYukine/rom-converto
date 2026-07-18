import {
	basename,
	deriveChdPath,
	deriveCompressedPath,
	deriveCsoPath,
	deriveNszPath,
	deriveRvzPath,
	withOutputDir,
} from "~/composables/useDerivedPath";
import { useChdCompressStore } from "~/stores/chd-compress";
import { useCsoCompressStore } from "~/stores/cso-compress";
import { useCtrCompressStore } from "~/stores/ctr-compress";
import { useDolCompressStore } from "~/stores/dol-compress";
import { isXciInput, useNxCompressStore } from "~/stores/nx-compress";
import { useRvlCompressStore } from "~/stores/rvl-compress";
import { useWupCompressStore } from "~/stores/wup-compress";
import { recursiveFields, registerOp, templateIsActive, type OpStore, type OutputRow } from "./types";

const ARCHIVE_EXTS = ["zip", "7z", "rar", "tar", "tgz", "gz"];

function outDir(store: OpStore): string {
	return typeof store.outputDir === "string" ? store.outputDir : "";
}

function outPath(store: OpStore, derived: string): string | null {
	if (templateIsActive(store)) return null;
	return withOutputDir(derived, outDir(store)) || null;
}

const CHUNK_SIZES = [32768, 65536, 131072, 262144, 524288, 1048576, 2097152];
const CHUNK_LABELS: Record<number, string> = {
	32768: "32 KiB",
	65536: "64 KiB",
	131072: "128 KiB (Dolphin default)",
	262144: "256 KiB",
	524288: "512 KiB",
	1048576: "1 MiB",
	2097152: "2 MiB",
};

function chunkLabel(bytes: number): string {
	return CHUNK_LABELS[bytes] ?? `${bytes} B`;
}

function cycleChunk(store: OpStore): void {
	const i = CHUNK_SIZES.indexOf(store.chunkSize);
	store.chunkSize = CHUNK_SIZES[(i + 1) % CHUNK_SIZES.length];
}

function pow2Label(exp: number): string {
	const bytes = 2 ** exp;
	return bytes >= 1048576 ? `${bytes / 1048576} MiB` : `${bytes / 1024} KiB`;
}

// Shared Output-card rows. `template`/`report` opt in per console since not
// every compress store carries those fields.
function outputRows(defaultDir: string, opts: { template?: boolean; report?: "field" | "static" }): OutputRow[] {
	const rows: OutputRow[] = [
		{
			kind: "directory",
			label: "Directory",
			color: "blue",
			display: (s) => s.outputDir || defaultDir,
			set: (s, v) => {
				s.outputDir = v;
			},
		},
	];
	if (opts.template) {
		rows.push({
			kind: "template",
			label: "Template",
			color: "blue",
			display: (s) => s.outputTemplate || "{console}/{title}.{ext}",
			set: (s, v) => {
				s.outputTemplate = v;
			},
		});
	}
	if (opts.report === "field") {
		rows.push({
			kind: "report",
			label: "Run report",
			display: (s) => (s.reportFile ? basename(s.reportFile) : "none"),
			set: (s, v) => {
				s.reportFile = v;
			},
		});
	} else if (opts.report === "static") {
		rows.push({ kind: "text", label: "Run report", display: () => "none" });
	}
	return rows;
}

export const CHD_CODEC_OPTIONS = [
	{ value: "cdlz", label: "CD LZMA" },
	{ value: "cdzl", label: "CD Deflate" },
	{ value: "cdfl", label: "CD FLAC" },
	{ value: "cdzs", label: "CD Zstandard" },
	{ value: "lzma", label: "LZMA" },
	{ value: "zlib", label: "Deflate" },
	{ value: "zstd", label: "Zstandard" },
	{ value: "huff", label: "Huffman" },
	{ value: "flac", label: "FLAC" },
];
export const CHD_DVD_CODEC_OPTIONS = CHD_CODEC_OPTIONS.filter((o) => !o.value.startsWith("cd"));
export const CHD_CODEC_PLACEHOLDER = "auto (cdlz, cdzl, cdfl for CD / lzma, zlib, huff, flac for DVD)";
export const CHD_DVD_ZSTD_HINT =
	"Consider adding Zstandard (zstd) for DVD images: better compression and faster decode, but rejected by AetherSX2/NetherSX2.";
export const CHD_LEVEL_HINT = "1-22; zstd uses it directly, zlib/lzma cap at 9; auto = zstd 19, lzma 8, zlib 9";

const discFields = (hint: string) => [
	{ kind: "slider" as const, key: "level", label: "Zstd level", min: 1, max: 22, hint },
	{
		kind: "kv" as const,
		key: "chunkSize",
		label: "Chunk size",
		tooltip: "--chunk-size · Chunks above 1 MiB can stutter on weaker hardware. 128 KiB is the safe default.",
		display: (s: OpStore) => chunkLabel(s.chunkSize),
		onClick: cycleChunk,
	},
	...recursiveFields(),
];

registerOp("compress", {
	nx: {
		op: "compress",
		console: "nx",
		opLabel: "nx compress",
		storeId: "nx-compress",
		useStore: useNxCompressStore,
		command: "cmd_nx_compress",
		resultKind: "convert",
		title: "Compress to NSZ / XCZ",
		subtitle: "Output is nsz-compatible. Requires prod.keys.",
		dropText: "Drop NSP or XCI files or folders. Archives (.zip, .7z, .rar) work too",
		acceptedExts: ["nsp", "xci", ...ARCHIVE_EXTS],
		browseFilters: [{ name: "Switch", extensions: ["nsp", "xci", ...ARCHIVE_EXTS] }],
		defaultOutputDir: "~/roms/switch/compressed",
		fields: [
			{
				kind: "slider",
				key: "level",
				label: "Zstd level",
				min: 1,
				max: 22,
				hint: "nsz default is 18. 22 is max but needs >1 GiB RAM to decompress on a Switch and may break installers.",
			},
			{
				kind: "segmented",
				key: "mode",
				label: "Mode",
				options: [
					{ label: "Solid", value: "solid" },
					{ label: "Block", value: "block" },
				],
				onSet: (s) => {
					s.userPickedMode = true;
				},
				hint: "Solid: one zstd frame per NCA (NSP default). Block: random-read friendly (XCI default).",
			},
			{
				kind: "slider",
				key: "blockSizeExp",
				label: "Block size",
				min: 14,
				max: 32,
				visible: (s) => s.mode === "block",
				formatValue: (v) => `2^${v} = ${pow2Label(v)}`,
			},
			{
				kind: "file",
				key: "keys",
				label: "prod.keys",
				filters: [{ name: "Keys", extensions: ["keys", "txt", "dat"] }],
				display: (s) => (s.keys ? `${basename(s.keys)} ✓` : "none"),
			},
			...recursiveFields(),
		],
		outputRows: outputRows("~/roms/switch/compressed", { template: true, report: "field" }),
		showVerify: true,
		verifyLabel: "Verify after conversion",
		actionNote:
			"Jobs start automatically. Parameters can't be changed after queuing. Remove and re-add instead.",
		// nsz defaults block mode for XCI; honor that unless the user picked a mode.
		onStaged: (store, items) => {
			if (!store.userPickedMode && items.some((i) => isXciInput(i.path))) store.mode = "block";
		},
		deriveOutput: (input) => deriveNszPath(input),
		buildArgs: (store, item, taskId) => ({
			input: item.path,
			output: outPath(store, deriveNszPath(item.path)),
			keys: store.keys || null,
			level: store.level,
			mode: store.mode,
			blockSizeExp: store.blockSizeExp,
			taskId,
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			report: !!store.reportFile,
			reportFile: store.reportFile || null,
			verifyAfter: store.verifyAfter,
		}),
		chips: (s) =>
			`level ${s.level} · ${s.mode}${s.mode === "block" ? ` · 2^${s.blockSizeExp}` : ""}`,
	},

	dol: {
		op: "compress",
		console: "dol",
		opLabel: "dol compress",
		storeId: "dol-compress",
		useStore: useDolCompressStore,
		command: "cmd_compress_disc",
		resultKind: "convert",
		title: "Compress to RVZ",
		subtitle: "Output is byte-identical to Dolphin at matching settings.",
		dropText: "Drop .iso, .gcm, .gcz or NKit files or folders",
		acceptedExts: ["iso", "gcm", "gcz", ...ARCHIVE_EXTS],
		browseFilters: [{ name: "GameCube", extensions: ["iso", "gcm", "gcz", ...ARCHIVE_EXTS] }],
		defaultOutputDir: "~/roms/gamecube/compressed",
		fields: discFields("1 is fastest, 22 is max ratio. Dolphin's documented suggestion is 5."),
		outputRows: outputRows("~/roms/gamecube/compressed", { template: true, report: "field" }),
		showVerify: true,
		verifyLabel: "Verify after conversion",
		actionNote:
			"Jobs start automatically. Parameters can't be changed after queuing. Remove and re-add instead.",
		deriveOutput: (input) => deriveRvzPath(input),
		buildArgs: (store, item, taskId) => ({
			input: item.path,
			output: outPath(store, deriveRvzPath(item.path)),
			level: store.level,
			chunkSize: store.chunkSize,
			taskId,
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			report: !!store.reportFile,
			reportFile: store.reportFile || null,
			verifyAfter: store.verifyAfter,
		}),
		chips: (s) => `level ${s.level} · ${chunkLabel(s.chunkSize).split(" (")[0]}`,
	},

	rvl: {
		op: "compress",
		console: "rvl",
		opLabel: "rvl compress",
		storeId: "rvl-compress",
		useStore: useRvlCompressStore,
		command: "cmd_compress_disc",
		resultKind: "convert",
		title: "Compress to RVZ",
		subtitle: "Output is byte-identical to Dolphin at matching settings.",
		dropText: "Drop .iso, .wbfs, .wia or .gcz files or folders",
		acceptedExts: ["iso", "wbfs", "wia", "gcz", ...ARCHIVE_EXTS],
		browseFilters: [{ name: "Wii", extensions: ["iso", "wbfs", "wia", "gcz", ...ARCHIVE_EXTS] }],
		defaultOutputDir: "~/roms/wii/compressed",
		fields: discFields("1 is fastest, 22 is max ratio. Dolphin's documented suggestion is 5."),
		outputRows: outputRows("~/roms/wii/compressed", { template: true, report: "field" }),
		showVerify: true,
		verifyLabel: "Verify after conversion",
		actionNote:
			"Jobs start automatically. Parameters can't be changed after queuing. Remove and re-add instead.",
		deriveOutput: (input) => deriveRvzPath(input),
		buildArgs: (store, item, taskId) => ({
			input: item.path,
			output: outPath(store, deriveRvzPath(item.path)),
			level: store.level,
			chunkSize: store.chunkSize,
			taskId,
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			report: !!store.reportFile,
			reportFile: store.reportFile || null,
			verifyAfter: store.verifyAfter,
		}),
		chips: (s) => `level ${s.level} · ${chunkLabel(s.chunkSize).split(" (")[0]}`,
	},

	ctr: {
		op: "compress",
		console: "ctr",
		opLabel: "ctr compress",
		storeId: "ctr-compress",
		useStore: useCtrCompressStore,
		command: "cmd_compress_rom",
		resultKind: "convert",
		title: "Compress to Z3DS",
		subtitle: "Output loads in Azahar.",
		dropText: "Drop .3ds, .cci or .cia files or folders",
		acceptedExts: ["cia", "cci", "3ds", "cxi", "3dsx", ...ARCHIVE_EXTS],
		browseFilters: [{ name: "3DS", extensions: ["cia", "cci", "3ds", "cxi", "3dsx", ...ARCHIVE_EXTS] }],
		defaultOutputDir: "~/roms/3ds/compressed",
		fields: [
			{
				kind: "slider",
				key: "level",
				label: "Zstd level",
				min: 0,
				max: 22,
				hint: "0 uses the library default. 1 is fastest, 22 is max ratio.",
				formatValue: (v) => (v === 0 ? "default (0)" : String(v)),
			},
			{
				kind: "toggle",
				key: "allowEncrypted",
				label: "Allow encrypted input",
				note: (s) =>
					s.allowEncrypted &&
					"Compresses even if the ROM looks encrypted. Encrypted 3DS ROMs barely compress — ctr decrypt first for real savings.",
			},
			...recursiveFields(),
		],
		outputRows: outputRows("~/roms/3ds/compressed", { template: true, report: "static" }),
		actionNote:
			"Jobs start automatically. Parameters can't be changed after queuing. Remove and re-add instead.",
		deriveOutput: (input) => deriveCompressedPath(input),
		buildArgs: (store, item, taskId) => ({
			input: item.path,
			output: outPath(store, deriveCompressedPath(item.path)),
			level: store.level,
			allowEncrypted: store.allowEncrypted,
			taskId,
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
		}),
		chips: (s) => `level ${s.level}${s.allowEncrypted ? " · allow-encrypted" : ""}`,
	},

	chd: {
		op: "compress",
		console: "chd",
		opLabel: "chd compress",
		storeId: "chd-compress",
		useStore: useChdCompressStore,
		command: "cmd_chd_compress",
		resultKind: "convert",
		title: "Compress to CHD",
		subtitle: "Output matches chdman createcd/createdvd.",
		dropText: "Drop .cue+.bin pairs or .iso files",
		acceptedExts: ["cue", "iso", ...ARCHIVE_EXTS],
		browseFilters: [{ name: "Disc image", extensions: ["cue", "iso", ...ARCHIVE_EXTS] }],
		defaultOutputDir: "~/roms/chd",
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
				hint: "Auto probes CD vs DVD from the image. Override only when detection is wrong.",
			},
			{ kind: "number", key: "hunkSize", label: "Hunk size", placeholder: "auto" },
			{
				kind: "multiselect",
				key: "codecs",
				label: "Codecs",
				options: CHD_CODEC_OPTIONS,
				max: 4,
				placeholder: CHD_CODEC_PLACEHOLDER,
				visible: (s) => s.mode !== "dvd",
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
			},
			{ kind: "number", key: "level", label: "Level", placeholder: "auto", hint: CHD_LEVEL_HINT },
			...recursiveFields(),
		],
		outputRows: outputRows("~/roms/chd", { template: true, report: "field" }),
		showVerify: true,
		verifyLabel: "Verify after conversion",
		actionNote:
			"Jobs start automatically. Parameters can't be changed after queuing. Remove and re-add instead.",
		deriveOutput: (input) => deriveChdPath(input),
		buildArgs: (store, item, taskId) => ({
			inputPath: item.path,
			output: outPath(store, deriveChdPath(item.path)),
			taskId,
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
		}),
		chips: (s) =>
			`${s.mode}${s.codecs.length ? ` · ${s.codecs.join(", ")}` : ""}${s.level ? ` · level ${s.level}` : ""}${s.hunkSize ? ` · hunk ${s.hunkSize}` : ""}`,
	},

	cso: {
		op: "compress",
		console: "cso",
		opLabel: "cso compress",
		storeId: "cso-compress",
		useStore: useCsoCompressStore,
		command: "cmd_cso_compress",
		resultKind: "convert",
		title: "Compress to CSO / ZSO",
		subtitle: "Output is maxcso-compatible.",
		dropText: "Drop .iso files or folders",
		acceptedExts: ["iso", ...ARCHIVE_EXTS],
		browseFilters: [{ name: "ISO", extensions: ["iso", ...ARCHIVE_EXTS] }],
		defaultOutputDir: "~/roms/psp/compressed",
		fields: [
			{
				kind: "segmented",
				key: "format",
				label: "Format",
				options: [
					{ label: "CSO", value: "cso" },
					{ label: "ZSO", value: "zso" },
				],
				hint: "CSO for PSP hardware and PPSSPP. ZSO for PS2 via Open PS2 Loader.",
			},
			{ kind: "number", key: "blockSize", label: "Block size", placeholder: "default" },
			...recursiveFields(),
		],
		outputRows: outputRows("~/roms/psp/compressed", { template: true, report: "field" }),
		showVerify: true,
		verifyLabel: "Verify after conversion",
		actionNote:
			"Jobs start automatically. Parameters can't be changed after queuing. Remove and re-add instead.",
		deriveOutput: (input, store) => deriveCsoPath(input, store.format),
		buildArgs: (store, item, taskId) => ({
			inputPath: item.path,
			output: outPath(store, deriveCsoPath(item.path, store.format)),
			format: store.format,
			taskId,
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			blockSize: store.blockSize || null,
			outputTemplate: store.outputTemplate || null,
			report: !!store.reportFile,
			reportFile: store.reportFile || null,
			verifyAfter: store.verifyAfter,
		}),
		chips: (s) => `${s.format}${s.blockSize ? ` · block ${s.blockSize}` : ""}`,
	},

	// Rendered by BundleView (see pages/[op]/[console].vue), not OpPage: Wii U
	// bundles are N inputs to one .wua, grouped by title ID.
	wup: {
		op: "compress",
		console: "wup",
		opLabel: "wup compress",
		storeId: "wup-compress",
		useStore: useWupCompressStore,
		command: "cmd_wup_compress",
		resultKind: "convert",
		title: "Bundle to WUA",
		subtitle:
			"Packs base game, update and DLC into one Cemu-ready .wua archive. One bundle, one job.",
		dropText:
			"Drop NUS or loadiine title folders, or .wud / .wux disc images. Base, update and DLC are detected by title ID",
		acceptedExts: ["wud", "wux"],
		browseFilters: [{ name: "Disc image", extensions: ["wud", "wux"] }],
		defaultOutputDir: "~/roms/wiiu/bundled",
		progressKey: "wup-compress",
		fields: [],
		outputRows: [],
		actionNote:
			"Each bundle is one queue job producing one .wua. The orphan DLC stays staged until you add its base or remove it.",
		buildArgs: (store) => ({
			inputs: [],
			output: store.output,
			level: store.level,
			keys: [],
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
		}),
		chips: (s) => `level ${s.level}`,
	},
});

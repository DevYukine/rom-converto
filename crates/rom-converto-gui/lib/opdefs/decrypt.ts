import { recursiveFields, registerOp, templateIsActive, type OpDef } from "./types";
import { useCtrDecryptStore } from "~/stores/ctr-decrypt";
import { useWupDecryptStore } from "~/stores/wup-decrypt";
import { usePs3DecryptStore } from "~/stores/ps3-decrypt";
import { basename, deriveDecryptedPath, withOutputDir } from "~/composables/useDerivedPath";

const ARCHIVE_EXTS = ["zip", "7z", "rar", "tar", "tgz", "gz"];

function deriveDecryptedWupPath(input: string): string {
	const trimmed = input.replace(/[\\/]+$/, "");
	return `${trimmed}_decrypted`;
}

const ctr: OpDef = {
	op: "decrypt",
	console: "ctr",
	opLabel: "Decrypt",
	storeId: "ctr-decrypt",
	useStore: useCtrDecryptStore,
	command: "cmd_decrypt_rom",
	resultKind: "convert",
	title: "Decrypt 3DS ROMs",
	subtitle: "Removes encryption for emulator use.",
	dropText: "Drop encrypted .3ds, .cci or .cia files or folders. Encryption state is detected automatically",
	acceptedExts: ["cia", "3ds", "cci", "cxi", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "3DS", extensions: ["cia", "3ds", "cci", "cxi"] }],
	fields: [
		{
			kind: "kv",
			key: "accepts",
			label: "Accepts",
			display: () => ".cia .3ds .cci .cxi",
			tooltip: "The format is auto detected from the file contents, so any of these can be dropped in.",
		},
		{
			kind: "kv",
			key: "seeddb",
			label: "seeddb.bin",
			display: () => "found next to app ✓",
			color: "green",
			tooltip: "Seeds needed for some titles resolve locally from seeddb.bin, falling back to Nintendo's API.",
		},
		...recursiveFields(),
	],
	note: "Format and encryption state are detected automatically. Seeds resolve locally from seeddb.bin, falling back to Nintendo's API.",
	outputRows: [
		{
			kind: "directory",
			label: "Directory",
			display: (s) => s.outputDir || "same as source",
			set: (s, v) => { s.outputDir = v; },
			tooltip: "Where the decrypted file is written. Leave empty to write it next to the source file.",
		},
		{
			kind: "text",
			label: "Filename",
			display: () => "{name}.decrypted.{ext}",
			tooltip: "The suffix keeps the output from colliding with the source.",
		},
	],
	showVerify: true,
	verifyLabel: "Verify after decryption",
	actionNote: "Already-decrypted files are skipped automatically and never queued.",
	deriveOutput: (input) => deriveDecryptedPath(input),
	buildArgs: (store, item, taskId) => {
		const tmpl = templateIsActive(store);
		return {
			input: item.path,
			output: tmpl ? null : withOutputDir(deriveDecryptedPath(item.path), store.outputDir || ""),
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			dryRun: false,
			taskId,
		};
	},
	chips: () => "",
};

const wup: OpDef = {
	op: "decrypt",
	console: "wup",
	opLabel: "Decrypt",
	storeId: "wup-decrypt",
	useStore: useWupDecryptStore,
	command: "cmd_wup_decrypt",
	resultKind: "convert",
	title: "Decrypt NUS title",
	subtitle:
		"Decrypts a Wii U NUS directory into a loadiine-shaped meta/code/content tree Cemu can install or load directly.",
	dropText: "Drop a NUS title directory (title.tmd + title.tik + .app, or the tmd.<N> community layout)",
	acceptedExts: [],
	singleInput: true,
	browseDirectory: true,
	progressKey: "wup-decrypt",
	fields: [
		{
			kind: "kv",
			key: "output",
			label: "Output",
			display: () => "meta/code/content tree",
			tooltip: "The decrypted title is written as a loadiine style folder tree that Cemu can load directly.",
		},
		{
			kind: "kv",
			key: "titleKey",
			label: "Title key",
			display: () => "derived when no ticket",
			tooltip: "Title key is derived from the title id when no ticket is present.",
		},
	],
	note: "Canonical NUS layouts (title.tmd + title.tik + {id}.app) and community layouts (tmd.<N> + optional cetk.<N>) both work.",
	outputRows: [
		{
			kind: "directory",
			label: "Directory",
			display: (s) => s.output || "<input>_decrypted",
			set: (s, v) => { s.output = v; },
			tooltip: "Where the decrypted folder tree is written. Created if missing. Leave empty to use the input name with a _decrypted suffix.",
		},
		{
			kind: "text",
			label: "Layout",
			display: () => "meta / code / content",
			tooltip: "The output is split into meta, code, and content folders, the layout Cemu expects.",
		},
	],
	renameDisabled: true,
	actionNote: "Already-decrypted files are skipped automatically and never queued.",
	buildArgs: (store, item) => ({
		input: item.path,
		output: store.output || deriveDecryptedWupPath(item.path),
		onConflict: store.onConflict,
		skipSpaceCheck: store.skipSpaceCheck,
		dryRun: false,
	}),
	chips: () => "",
};

const ps3: OpDef = {
	op: "decrypt",
	console: "ps3",
	opLabel: "Decrypt",
	storeId: "ps3-decrypt",
	useStore: usePs3DecryptStore,
	command: "cmd_ps3_decrypt",
	resultKind: "convert",
	title: "Decrypt PS3 ISO",
	subtitle: "Removes disc encryption for emulator use.",
	dropText: "Drop encrypted .iso files or folders. Uses the built-in key database or a sibling .dkey if no key is set",
	acceptedExts: ["iso", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "PS3 ISO", extensions: ["iso"] }],
	fields: [
		{
			kind: "file",
			key: "key",
			label: "Disc key (.dkey)",
			filters: [{ name: "Disc key", extensions: ["dkey"] }],
			display: (s) => (s.key ? `${basename(s.key)} ✓` : "sibling .dkey"),
			tooltip: "The disc's 16-byte data key. When left empty, it is looked up in the built-in database by the disc's title ID, then a sibling <input>.dkey next to the ISO.",
		},
		{
			kind: "toggle",
			key: "skipProbe",
			label: "Skip verification probe",
			tooltip:
				"Skips the encryption and disc-key check before converting. Use if a correct key is rejected because the disc's sampled sectors are all compressed data.",
		},
		...recursiveFields(),
	],
	note: "The data key is resolved from the key field above, else the built-in database by title ID, else a sibling <input>.dkey.",
	outputRows: [
		{
			kind: "directory",
			label: "Directory",
			display: (s) => s.outputDir || "same as source",
			set: (s, v) => { s.outputDir = v; },
			tooltip: "Where the decrypted file is written. Leave empty to write it next to the source file.",
		},
		{
			kind: "text",
			label: "Filename",
			display: () => "{name}.decrypted.{ext}",
			tooltip: "The suffix keeps the output from colliding with the source.",
		},
	],
	actionNote: "Already-decrypted files are detected and skipped during conversion.",
	deriveOutput: (input) => deriveDecryptedPath(input, "iso"),
	buildArgs: (store, item, taskId) => {
		const tmpl = templateIsActive(store);
		return {
			input: item.path,
			output: tmpl ? null : withOutputDir(deriveDecryptedPath(item.path, "iso"), store.outputDir || ""),
			key: store.key || null,
			skipProbe: store.skipProbe,
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			dryRun: false,
			taskId,
		};
	},
	chips: (store) => (store.key ? "key set" : "no key"),
};

registerOp("decrypt", { ctr, wup, ps3 });

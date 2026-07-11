import { recursiveFields, registerOp, templateIsActive, type OpDef } from "./types";
import { useCtrDecryptStore } from "~/stores/ctr-decrypt";
import { useWupDecryptStore } from "~/stores/wup-decrypt";
import { deriveDecryptedPath, withOutputDir } from "~/composables/useDerivedPath";

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
		{ kind: "kv", key: "accepts", label: "Accepts", display: () => ".cia .3ds .cci .cxi" },
		{ kind: "kv", key: "seeddb", label: "seeddb.bin", display: () => "found next to app ✓", color: "green" },
		...recursiveFields(),
	],
	note: "Format and encryption state are detected automatically. Seeds resolve locally from seeddb.bin, falling back to Nintendo's API.",
	outputRows: [
		{
			kind: "directory",
			label: "Directory",
			display: (s) => s.outputDir || "same as source",
			set: (s, v) => { s.outputDir = v; },
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
	deriveOutput: deriveDecryptedPath,
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
		{ kind: "kv", key: "output", label: "Output", display: () => "meta/code/content tree" },
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
		},
		{ kind: "text", label: "Layout", display: () => "meta / code / content" },
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

registerOp("decrypt", { ctr, wup });

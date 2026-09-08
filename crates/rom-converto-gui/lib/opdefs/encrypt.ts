import { commonOptions, recursiveFields, runArgs, templateIsActive, type OpDef } from "./types";
import { useCtrEncryptStore } from "~/stores/ctr-encrypt";
import { useNdsEncryptStore } from "~/stores/nds-encrypt";
import { deriveEncryptedPath, withOutputDir } from "~/composables/useDerivedPath";

const ARCHIVE_EXTS = ["zip", "7z", "rar", "tar", "tgz", "gz"];

const ctr: OpDef = {
	op: "encrypt",
	console: "ctr",
	opLabel: "Encrypt",
	storeId: "ctr-encrypt",
	useStore: useCtrEncryptStore,
	command: "cmd_run",
	resultKind: "convert",
	title: "Encrypt 3DS ROMs",
	subtitle: "Restores standard encryption on decrypted dumps.",
	dropText: "Drop decrypted .3ds, .cci or .cia files or folders. Encryption state is detected automatically",
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
		...recursiveFields(),
	],
	note: "CIA TMD hashes and content flags are rewritten as content is wrapped with the ticket title key, so encrypted bytes may differ from the original source, though decrypting still returns the same plaintext.",
	outputRows: [
		{
			kind: "directory",
			label: "Directory",
			display: (s) => s.outputDir || "same as source",
			set: (s, v) => { s.outputDir = v; },
			tooltip: "Where the encrypted file is written. Leave empty to write it next to the source file.",
		},
		{
			kind: "text",
			label: "Filename",
			display: () => "{name}.encrypted.{ext}",
			tooltip: "The suffix keeps the output from colliding with the source, same as {name}.decrypted.{ext} for decryption.",
		},
	],
	actionNote: "Already-encrypted files are skipped automatically and never queued.",
	deriveOutput: deriveEncryptedPath,
	buildArgs: (store, item, taskId) =>
		runArgs(
			"ctr.encrypt",
			item.path,
			templateIsActive(store) ? null : withOutputDir(deriveEncryptedPath(item.path), store.outputDir || ""),
			commonOptions(store),
			false,
			taskId,
		),
	chips: () => "",
};

const nds: OpDef = {
	op: "encrypt",
	console: "nds",
	opLabel: "Encrypt",
	storeId: "nds-encrypt",
	useStore: useNdsEncryptStore,
	command: "cmd_run",
	resultKind: "convert",
	title: "Encrypt Nintendo DS ROMs",
	subtitle: "Restores standard encryption on decrypted dumps.",
	dropText: "Drop decrypted .nds files or folders. Encryption state is detected automatically",
	acceptedExts: ["nds", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "Nintendo DS", extensions: ["nds"] }],
	fields: [...recursiveFields()],
	note: "Only the KEY1 secure area is rewritten; homebrew ROMs without a secure area are skipped.",
	outputRows: [
		{
			kind: "directory",
			label: "Directory",
			display: (s) => s.outputDir || "same as source",
			set: (s, v) => { s.outputDir = v; },
			tooltip: "Where the encrypted file is written. Leave empty to write it next to the source file.",
		},
		{
			kind: "text",
			label: "Filename",
			display: () => "{name}.encrypted.{ext}",
			tooltip: "The suffix keeps the output from colliding with the source, same as {name}.decrypted.{ext} for decryption.",
		},
	],
	actionNote: "Already-encrypted files are skipped automatically and never queued.",
	deriveOutput: deriveEncryptedPath,
	buildArgs: (store, item, taskId) =>
		runArgs(
			"nds.encrypt",
			item.path,
			templateIsActive(store) ? null : withOutputDir(deriveEncryptedPath(item.path), store.outputDir || ""),
			commonOptions(store),
			false,
			taskId,
		),
	chips: () => "",
};

export const encryptOps: OpDef[] = [ctr, nds];

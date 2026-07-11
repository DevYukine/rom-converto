import { recursiveFields, registerOp, templateIsActive, type OpDef } from "./types";
import { useCtrEncryptStore } from "~/stores/ctr-encrypt";
import { deriveEncryptedPath, withOutputDir } from "~/composables/useDerivedPath";

const ARCHIVE_EXTS = ["zip", "7z", "rar", "tar", "tgz", "gz"];

const ctr: OpDef = {
	op: "encrypt",
	console: "ctr",
	opLabel: "Encrypt",
	storeId: "ctr-encrypt",
	useStore: useCtrEncryptStore,
	command: "cmd_encrypt_rom",
	resultKind: "convert",
	title: "Encrypt 3DS ROMs",
	subtitle: "Restores standard encryption on decrypted dumps.",
	dropText: "Drop decrypted .3ds, .cci or .cia files or folders. Encryption state is detected automatically",
	acceptedExts: ["cia", "3ds", "cci", "cxi", ...ARCHIVE_EXTS],
	browseFilters: [{ name: "3DS", extensions: ["cia", "3ds", "cci", "cxi"] }],
	fields: [
		{ kind: "kv", key: "accepts", label: "Accepts", display: () => ".cia .3ds .cci .cxi" },
		...recursiveFields(),
	],
	note: "CIA TMD hashes and content flags are rewritten as content is wrapped with the ticket title key, so encrypted bytes may differ from the original source, though decrypting still returns the same plaintext.",
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
			display: () => "{name}.encrypted.{ext}",
			tooltip: "The suffix keeps the output from colliding with the source, same as {name}.decrypted.{ext} for decryption.",
		},
	],
	showVerify: true,
	verifyLabel: "Verify after encryption",
	actionNote: "Already-encrypted files are skipped automatically and never queued.",
	deriveOutput: deriveEncryptedPath,
	buildArgs: (store, item, taskId) => {
		const tmpl = templateIsActive(store);
		return {
			input: item.path,
			output: tmpl ? null : withOutputDir(deriveEncryptedPath(item.path), store.outputDir || ""),
			onConflict: store.onConflict,
			skipSpaceCheck: store.skipSpaceCheck,
			outputTemplate: store.outputTemplate || null,
			dryRun: false,
			taskId,
		};
	},
	chips: () => "",
};

registerOp("encrypt", { ctr });

import { useOrganizeStore } from "~/stores/organize";
import { nxKeysColor, nxKeysDisplay } from "./nx-keys";
import { NX_KEYS_TOOLTIP, runArgs, type FieldDef, type OpDef } from "./types";

const OUTPUT_TEMPLATE_DEFAULT = "{console}/{basename}.{ext}";

const fields: FieldDef[] = [
	{
		kind: "text",
		key: "outputTemplate",
		label: "Layout",
		placeholder: OUTPUT_TEMPLATE_DEFAULT,
		tooltip: "Path layout under the output directory. Tokens: {console} {title} {titleId} {region} {serial} {basename} {ext}.",
	},
	{
		kind: "toggle",
		key: "dat",
		label: "Rename with DAT (online)",
		tooltip:
			"Matches each file against the online Playmatch database and renames it to the canonical name before filing. Even a dry run hashes every file and queries the API.",
	},
	{
		kind: "toggle",
		key: "moveSource",
		label: "Move (delete sources after success)",
		tooltip: "Deletes each source file after its organized copy was written successfully. Nothing is deleted for skipped or failed files.",
	},
	{
		kind: "toggle",
		key: "playlists",
		label: "Write .m3u playlists",
		tooltip: "Writes an .m3u for every multi-disc set in the output folders. Playlists are written on real runs only, never on a dry run.",
	},
	{
		kind: "toggle",
		key: "allowEncrypted",
		label: "Compress encrypted 3DS ROMs",
		tooltip: "Compresses encrypted 3DS ROMs directly instead of requiring decrypted dumps.",
	},
	{
		kind: "number",
		key: "maxDepth",
		label: "Max depth",
		placeholder: "Unlimited",
		tooltip: "Folder levels to descend when scanning the dropped library. Leave empty for unlimited.",
	},
	{
		kind: "file",
		key: "keys",
		label: "prod.keys",
		tooltip: NX_KEYS_TOOLTIP,
		filters: [{ name: "Keys", extensions: ["keys", "txt", "dat"] }],
		display: nxKeysDisplay,
		color: nxKeysColor,
	},
];

export const organizeOps: OpDef[] = [
	{
		op: "organize",
		console: "library",
		opLabel: "organize",
		storeId: "organize",
		useStore: useOrganizeStore,
		command: "cmd_run",
		resultKind: "organize",
		progressKey: "organize",
		title: "Organize a library",
		subtitle:
			"Sort a ROM folder into per-console folders and compress every file into its best format",
		dropText: "Drop a library folder to organize",
		acceptedExts: [],
		singleInput: true,
		browseDirectory: true,
		fields,
		outputRows: [
			{
				kind: "directory",
				label: "Output directory",
				display: (store) => store.outputDir || "required",
				set: (store, value) => {
					store.outputDir = value;
				},
				tooltip: "Root folder that receives the per-console subfolders.",
			},
		],
		showConflict: true,
		showDryRun: true,
		actionNote: "Runs in the global queue. Rows appear below as they stream in.",
		buildArgs: (store, item, taskId) =>
			runArgs(
				"organize",
				item.path,
				null,
				{
					output_dir: store.outputDir || null,
					output_template: store.outputTemplate || null,
					dat: store.dat,
					move_source: store.moveSource,
					playlists: store.playlists,
					allow_encrypted: store.allowEncrypted,
					max_depth: store.maxDepth,
					keys: store.keys || null,
					on_conflict: store.onConflict,
					skip_space_check: store.skipSpaceCheck,
				},
				false,
				taskId,
			),
		chips: () => "RVZ · CHD · NSZ · CSO · ZIP",
	},
];

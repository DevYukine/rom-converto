import { useDatScanStore } from "~/stores/datScan";
import type { ScanLevel } from "~/stores/datScan";
import { useDatVerifyStore } from "~/stores/datVerify";
import { useDatRenameStore } from "~/stores/datRename";
import { dryRunArgs, runArgs, type FieldDef, type OpDef } from "./types";

// Every level keeps crc32: it is near-free alongside the stronger digest and
// stays the fallback match rung.
const SCAN_LEVEL_ALGOS: Record<ScanLevel, string> = {
	crc: "crc32",
	md5: "crc32,md5",
	sha1: "crc32,sha1",
	sha256: "crc32,sha256",
};

const MAX_DEPTH: FieldDef = {
	kind: "number",
	key: "maxDepth",
	label: "Max depth",
	placeholder: "Unlimited",
	tooltip: "Folder levels to scan. Leave it empty for unlimited.",
};

const QUICK_TOOLTIP =
	"Trusts a zip's own CRC32 for eligible cartridge images instead of extracting and hashing. Falls back automatically when that alone does not verify.";

export const datOps: OpDef[] = [
	{
		op: "dat",
		console: "scan",
		opLabel: "dat scan",
		storeId: "dat-scan",
		useStore: useDatScanStore,
		command: "cmd_run",
		resultKind: "datScan",
		progressKey: "dat-scan",
		title: "Scan library",
		subtitle:
			"Matches each file against the Playmatch DAT database, streaming results live. Cancel keeps partial results.",
		dropText: "Drop a folder to scan",
		acceptedExts: [],
		singleInput: true,
		browseDirectory: true,
		fields: [
			{
				kind: "segmented",
				key: "scanLevel",
				label: "Level",
				options: [
					{ label: "CRC + Size", value: "crc" },
					{ label: "MD5", value: "md5" },
					{ label: "SHA-1", value: "sha1" },
					{ label: "SHA-256", value: "sha256" },
				],
				hint: "Quick scan trusts zip CRC32 where possible and falls back automatically.",
				tooltip:
					"CRC32 plus size identifies almost everything. Raise this to MD5, SHA-1, or SHA-256 only when a match needs a stronger digest.",
			},
			{
				kind: "toggle",
				key: "quick",
				label: "Quick scan",
				tooltip: QUICK_TOOLTIP,
			},
			MAX_DEPTH,
		],
		outputRows: [],
		showConflict: false,
		showDryRun: false,
		actionNote: "Scans run in the global queue. Rows appear below as they stream in.",
		buildArgs: (store, item, taskId) =>
			runArgs(
				"dat.scan",
				item.path,
				null,
				{
					algo: SCAN_LEVEL_ALGOS[store.scanLevel as ScanLevel],
					quick: store.quick,
					max_depth: store.maxDepth,
				},
				false,
				taskId,
			),
		chips: (store) => `${store.scanLevel}${store.quick ? " · quick" : ""}`,
	},
	{
		op: "dat",
		console: "verify",
		opLabel: "dat verify",
		storeId: "dat-verify",
		useStore: useDatVerifyStore,
		command: "cmd_run",
		resultKind: "datVerify",
		progressKey: "dat-verify",
		title: "Verify library",
		subtitle: "Confirms each file's full hash matches its Playmatch DAT entry. Slower and stronger than a scan.",
		dropText: "Drop ROM files or a folder to verify",
		acceptedExts: [],
		browseFilters: [{ name: "ROM file", extensions: ["*"] }],
		browseAlsoDirectory: true,
		fields: [
			{
				kind: "toggle",
				key: "quick",
				label: "Quick verify",
				description: "Trust a zip's own CRC32 for eligible cartridge images instead of extracting and hashing",
				tooltip: QUICK_TOOLTIP,
			},
		],
		outputRows: [],
		showConflict: false,
		showDryRun: false,
		actionNote: "Verify jobs run in the global queue. Results appear below as they finish.",
		buildArgs: (store, item, taskId) =>
			runArgs("dat.verify", item.path, null, { quick: store.quick }, false, taskId),
		chips: (store) => (store.quick ? "quick" : "full"),
	},
	{
		op: "dat",
		console: "rename",
		opLabel: "dat rename",
		storeId: "dat-rename",
		useStore: useDatRenameStore,
		command: "cmd_run",
		resultKind: "datRename",
		progressKey: "dat-rename",
		title: "Rename to canonical",
		subtitle: "Renames files to their canonical Playmatch DAT names. Only hash-verified matches are renamed.",
		dropText: "Drop a folder to preview renames",
		acceptedExts: [],
		singleInput: true,
		browseDirectory: true,
		fields: [MAX_DEPTH],
		outputRows: [],
		showDryRun: false,
		actionNote: "Queues a preview. Apply the renames from the plan below.",
		// The queued job is the preview; the plan card re-queues it without the
		// dry-run flag so the filesystem is re-planned at apply time.
		buildArgs: (store, item, taskId) =>
			dryRunArgs(
				runArgs(
					"dat.rename",
					item.path,
					null,
					{ max_depth: store.maxDepth, on_conflict: store.onConflict },
					false,
					taskId,
				),
			),
		chips: (store) => `on conflict: ${store.onConflict}`,
	},
];

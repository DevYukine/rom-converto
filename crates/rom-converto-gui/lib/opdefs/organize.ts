import { useOrganizeStore } from "~/stores/organize";
import { nxKeysColor, nxKeysDisplay } from "./nx-keys";
import { NX_KEYS_TOOLTIP, runArgs, type FieldDef, type OpDef } from "./types";

const OUTPUT_TEMPLATE_DEFAULT = "{console}/{basename}.{ext}";

// Code lists are one comma-separated text input in the store; this splits
// them into the runner's `string[]`. Globs and regexes never go through
// here: they are one entry per line (splitLines), so a comma inside a
// pattern survives.
function splitList(value: string): string[] | undefined {
	const items = value.split(",").map((v) => v.trim()).filter(Boolean);
	return items.length ? items : undefined;
}

// Regex, path, and glob values may contain commas, so they are one entry per
// line.
function splitLines(value: string): string[] | undefined {
	const items = value.split(/\r?\n/).map((v) => v.trim()).filter(Boolean);
	return items.length ? items : undefined;
}

const fields: FieldDef[] = [
	{
		kind: "text",
		key: "outputTemplate",
		label: "Layout",
		placeholder: OUTPUT_TEMPLATE_DEFAULT,
		hint: "Blank uses the config file's layout.",
		tooltip: "Path layout under the output directory. Tokens: {console} {title} {titleId} {region} {serial} {basename} {ext} {language} {type} {dat} {game} {input_dir}, plus frontend tokens {adam} {batocera} {crossmix} {es} {funkeyos} {minui} {mister} {miyoocfw} {onion} {pocket} {retrodeck} {rocknix} {romm} {spruce} {twmenu}.",
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
		tooltip: "Deletes each source file after its organized copy was written successfully. Skipped or failed files keep their sources, and so do multi-entry archives (only single-entry archives are released), patch-only runs, and, with Verify-after on, conversions whose format has no output check. The exception: an existing zip, copy, or hardlink (never a symlink) that verifies valid under Overwrite if invalid counts as written and its source is deleted.",
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

	// --- Filters ---
	{
		kind: "text",
		key: "inputExclude",
		label: "Filter · Exclude files",
		placeholder: "One per line, e.g. **/*.{txt,pdf}",
		multiline: true,
		tooltip:
			"Globs matched against each file's path relative to the scanned folder (absolute globs work too), one per line. Matching files are dropped before anything else happens, and an excluded file is never cleaned or moved. A cue sheet with its bins, or a split .wud set, is dropped whole when any member matches. `*` stops at `/`, so a recursive match needs `**/`.",
	},
	{
		kind: "text",
		key: "filterRegex",
		label: "Filter · Name includes",
		placeholder: "One per line, e.g. (USA|Europe)",
		multiline: true,
		tooltip:
			"Keep only files whose DAT game name, or filename stem without a DAT match, matches one of these regular expressions, one per line. The /pattern/flags form is accepted.",
	},
	{
		kind: "text",
		key: "filterRegexExclude",
		label: "Filter · Name excludes",
		placeholder: "One per line, e.g. \\(Demo\\)",
		multiline: true,
		tooltip:
			"Drop files whose DAT game name, or filename stem without a DAT match, matches one of these regular expressions, one per line.",
	},
	{
		kind: "text",
		key: "filterLanguage",
		label: "Filter · Languages",
		placeholder: "EN, FR",
		tooltip:
			"Comma-separated two-letter language codes. Files whose DAT languages include none of these are skipped; a game with no language tag falls back to its region's primary language, and without a DAT match the tags are read from the file name.",
	},
	{
		kind: "text",
		key: "filterRegion",
		label: "Filter · Regions",
		placeholder: "USA, EUR, JPN, WORLD",
		tooltip:
			"Comma-separated region codes (USA, EUR, JPN, WORLD, …). Files whose DAT regions include none of these are skipped; without a DAT match the tags are read from the file name.",
	},
	{
		kind: "text",
		key: "noType",
		label: "Filter · Exclude types",
		placeholder: "bios, demo, beta",
		tooltip:
			"Skip files tagged as one of these DAT types: bios, device, unlicensed, debug, demo, beta, sample, prototype, program, aftermarket, homebrew, alpha, bootleg, cracked, fixed, hacked, overdump, pendingdump, pirated, trained, translated, bad, unverified. `unverified` matches names that lack the classic `[!]` marker, and `device` never matches anything.",
	},
	{
		kind: "text",
		key: "onlyType",
		label: "Filter · Only types",
		placeholder: "program, bios",
		tooltip: "Keep only files tagged as one of these DAT types (same names and semantics as the exclude field above).",
	},
	{
		kind: "toggle",
		key: "onlyRetail",
		label: "Filter · Retail only",
		tooltip:
			"Keeps only retail releases. Drops bios, device, alpha, bad, beta, bootleg, cracked, debug, demo, fixed, hacked, homebrew, overdump, pendingdump, pirated, program, prototype, sample, trained, translated, and aftermarket releases; unlicensed stays retail.",
	},

	// --- Best release ---
	{
		kind: "toggle",
		key: "single",
		label: "Best release · One game per set",
		description: "Keeps a single ROM per parent/clone group; the rest are skipped.",
		disabled: (s) => !s.dat,
		note: (s) => !s.dat && "Enable “Rename with DAT (online)” first.",
		tooltip:
			"Uses the online DAT match to group clones under their parent and keeps one ROM per group.",
	},
	{
		kind: "text",
		key: "preferRegion",
		label: "Best release · Region priority",
		placeholder: "USA, EUR, JPN",
		visible: (s) => !!s.single && !!s.dat,
		tooltip: "Priority order of region codes (USA, EUR, JPN, …); the first match wins its group.",
	},
	{
		kind: "text",
		key: "preferLanguage",
		label: "Best release · Language priority",
		placeholder: "EN, FR",
		visible: (s) => !!s.single && !!s.dat,
		tooltip: "Priority order of two-letter language codes; the first match wins its group.",
	},
	{
		kind: "segmented",
		key: "preferRevision",
		label: "Best release · Revision",
		options: [
			{ label: "Any", value: "" },
			{ label: "Older", value: "older" },
			{ label: "Newer", value: "newer" },
		],
		hint: "Any (default): the revision does not matter.",
		visible: (s) => !!s.single && !!s.dat,
		tooltip: "Prefer the older or newer revision within each group.",
	},
	{
		kind: "toggle",
		key: "preferRetail",
		label: "Best release · Prefer retail",
		visible: (s) => !!s.single && !!s.dat,
		tooltip: "Prefers retail releases over demos, betas, prototypes, hacks and other non-retail marks (unlicensed counts as retail).",
	},
	{
		kind: "toggle",
		key: "preferParent",
		label: "Best release · Prefer parent",
		visible: (s) => !!s.single && !!s.dat,
		tooltip: "Prefers the parent ROM of a clone group over any clone.",
	},
	{
		kind: "toggle",
		key: "preferVerified",
		label: "Best release · Prefer verified",
		visible: (s) => !!s.single && !!s.dat,
		tooltip:
			"Prefers dumps whose game name carries the classic `[!]` verified-dump marker; a name check, not hash verification.",
	},
	{
		kind: "toggle",
		key: "preferGood",
		label: "Best release · Prefer good",
		visible: (s) => !!s.single && !!s.dat,
		tooltip:
			"Prefers dumps whose game name lacks the classic `[b]` bad-dump marker when a clean copy exists.",
	},
	{
		kind: "text",
		key: "preferGameRegex",
		label: "Best release · Prefer name regex",
		placeholder: "One per line, e.g. Rev A",
		multiline: true,
		visible: (s) => !!s.single && !!s.dat,
		tooltip:
			"Regular expressions over the DAT game name, one per line; a game matching an earlier pattern wins its group.",
	},
	{
		kind: "text",
		key: "preferFilenameRegex",
		label: "Ties · Prefer filename regex",
		placeholder: "One per line, e.g. \\(v1\\.1\\)",
		multiline: true,
		tooltip:
			"Regular expressions over the filename, one per line; a file matching an earlier pattern wins. Breaks ties between inputs for the same game or the same output path, after the already-placed and format preferences, and works without Best release or a DAT match.",
	},

	// --- Layout ---
	{
		kind: "toggle",
		key: "dirLetter",
		label: "Folders · First-letter dirs",
		tooltip:
			"Files land in a subfolder named after their leading characters, like the letter folders other ROM managers build.",
	},
	{
		kind: "number",
		key: "dirLetterCount",
		label: "Folders · Characters per dir",
		placeholder: "1",
		min: 1,
		max: 26,
		visible: (s) => !!s.dirLetter,
		tooltip: "How many leading characters form the folder name (1 through 26).",
	},
	{
		kind: "number",
		key: "dirLetterLimit",
		label: "Folders · Dir limit",
		placeholder: "Unlimited",
		hint: "Blank follows the config file.",
		min: 1,
		visible: (s) => !!s.dirLetter,
		tooltip:
			"Caps how many items a letter folder holds; a letter that exceeds the cap is split into numbered folders (A1, A2, …). Blank follows the config file.",
	},
	{
		kind: "toggle",
		key: "dirLetterGroup",
		label: "Folders · Range grouping",
		visible: (s) => !!s.dirLetter,
		tooltip:
			"Merges adjacent under-full letter folders into ranges sized by the dir limit, such as A-C and D-F. Requires the dir limit.",
	},
	{
		kind: "toggle",
		key: "multiDiscDirs",
		label: "Folders · Per multi-disc game",
		tooltip:
			"Places every multi-disc set in its own folder named after the game. With playlists on, the .m3u is written inside that folder.",
	},

	// --- Archive ---
	{
		kind: "segmented",
		key: "zipFormat",
		label: "Archive · Zip format",
		options: [
			{ label: "Config default", value: "" },
			{ label: "TorrentZip", value: "torrentzip" },
			{ label: "RVZSTD", value: "rvzstd" },
		],
		hint: "Config default (TorrentZip when unset) follows the config file. TorrentZip writes deterministic zips; RVZSTD writes a zstd-compressed structured zip.",
		tooltip:
			"TorrentZip is the torrent-standard deterministic zip. RVZSTD is a zstd-compressed structured zip: smaller, but far fewer tools read it. Not the RVZ disc format. Config default follows the config file (TorrentZip when unset).",
	},
	{
		kind: "text",
		key: "zipExclude",
		label: "Archive · Skip zipping",
		placeholder: "NDS/**",
		tooltip:
			"Glob matched against the planned .zip output path (for example 'NDS/Game.zip'); matching files are placed as a plain copy (or a link when a link mode is set) instead of zipped. `*` stops at `/`, so a recursive match needs `**/`.",
	},

	// --- Link ---
	{
		kind: "segmented",
		key: "linkMode",
		label: "Link · Mode",
		options: [
			{ label: "Config default", value: "" },
			{ label: "Hardlink", value: "hardlink" },
			{ label: "Symlink", value: "symlink" },
			{ label: "Reflink", value: "reflink" },
		],
		hint: "Config default follows the config file (plain copy when unset). Link modes replace the copy for files already in their best format.",
		tooltip:
			"Instead of copying files that need no conversion, hardlink, symlink, or reflink them. Hardlinks and reflinks need the source and the output on the same filesystem, and reflinks also need a filesystem with copy-on-write clones (for example APFS, Btrfs, XFS, ReFS); elsewhere the item fails. Links are placed for plain, unpatched sources; archive members, patched variants and header-stripped/padded payloads are copied instead (the row detail says so). Symlinks can't be combined with move mode.",
	},
	{
		kind: "toggle",
		key: "symlinkRelative",
		label: "Link · Relative symlinks",
		visible: (s) => s.linkMode === "symlink",
		tooltip: "Writes symlinks with relative target paths instead of absolute ones.",
	},

	// --- ROM processing ---
	{
		kind: "text",
		key: "removeHeaders",
		label: "ROM · Strip headers",
		placeholder: "nes, smc, or “all”",
		tooltip:
			"Comma-separated extensions whose copier headers are stripped when writing (nes, fds, a78, lnx→lyx, smc/sfc→sfc). Use “all” to strip every detected header. With a DAT match, a file that matches only once its header is removed is written headerless whatever this field says; a match in a “(Headered)” DAT keeps the header.",
	},
	{
		kind: "toggle",
		key: "trimAddPadding",
		label: "ROM · Re-pad trimmed dumps",
		tooltip:
			"Trimmed GBA/NDS dumps are padded back to their official size. The default fill byte is 0xFF for unused retail GBA/NDS space; with a DAT match, a verified padded match uses its verified fill byte (0x00 or 0xFF).",
	},
	{
		kind: "text",
		key: "patch",
		label: "ROM · Patch files",
		placeholder: "~/patches, one path per line",
		multiline: true,
		tooltip:
			"Patch files or folders, one per line (.aps, .bps, .ebp, .ips, .ips32, .ppf, .rup, .ups, or .vcdiff/.xdelta (VCDIFF)). BPS and UPS carry the source CRC32; every other format pairs through the CRC32 in the patch file name: [XXXXXXXX], (XXXXXXXX), 0xXXXXXXXX, or a bare 8-hex run containing a letter A-F. The APS declared size and the .rup source and target MD5 are checked when the patch is applied.",
	},
	{
		kind: "toggle",
		key: "patchOnly",
		label: "ROM · Only patched copies",
		visible: (s) => !!s.patch.trim(),
		tooltip:
			"Writes only the patched variants; ROMs without a matching patch are skipped. The unpatched original always stays, even with Move.",
	},

	// --- Clean ---
	{
		kind: "toggle",
		key: "clean",
		label: "Clean · Remove stale files",
		tooltip:
			"After writing, deletes files in the written output folders that this run didn't produce. Never deletes files under INPUT, and a dry run only previews the deletions.",
	},
	{
		kind: "text",
		key: "cleanExclude",
		label: "Clean · Keep globs",
		placeholder: "One per line, e.g. **/*.txt or **/covers/**",
		multiline: true,
		visible: (s) => !!s.clean,
		tooltip:
			"Globs, relative to the output folder, whose matches are never deleted by clean. One per line. `*` stops at `/`, so a recursive match needs `**/`. Matching ignores letter case.",
	},
	{
		kind: "segmented",
		key: "moveDeleteDirs",
		label: "Move · Empty folders",
		options: [
			{ label: "Config default", value: "" },
			{ label: "Never", value: "never" },
			{ label: "Auto", value: "auto" },
			{ label: "Always", value: "always" },
		],
		hint: "Config default follows the config file (auto when unset). Always also removes empty folders that were already there.",
		visible: (s) => !!s.moveSource,
		tooltip:
			"With Move, when emptied source folders are deleted: never, auto (only the folders this run emptied), or always (every empty folder under INPUT, including ones that were already empty). Config default follows the config file (auto when unset). Needs Move.",
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
				value: (store) => store.outputDir,
				set: (store, value) => {
					store.outputDir = value;
				},
				tooltip: "Root folder that receives the per-console subfolders.",
			},
			{
				kind: "directory",
				label: "Clean backup folder",
				picker: { title: "Clean backup folder", clearLabel: "Config default" },
				display: (store) => store.cleanBackup || "config default",
				value: (store) => store.cleanBackup,
				set: (store, value) => {
					store.cleanBackup = value;
				},
				tooltip:
					"When set, files removed by Clean are moved here (flat, “ (n)” suffix on name collisions) instead of being deleted. Blank follows the config file. Dry runs preview what would be moved.",
			},
		],
		showConflict: true,
		showVerify: true,
		verifyLabel: "Verify after organize",
		verifyTooltip:
			"Re-verifies each written zip, copy, or converted output right after writing it: size and CRC32 for zips and copies plus TorrentZip or RVZSTD structure for zips, and the format's own verify for conversions. A failed check fails that item. Conversions whose format has no output check (3DS, Wii U, Xbox, Xbox 360, PS3) fail as unverified and keep their source.",
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
					// Every shown field sends an explicit value so a config
					// file cannot quietly apply behind the form: false for
					// off toggles, [] for empty lists, "any" for no revision
					// preference. The exceptions are deliberate: the
					// "Config default" options and a blank folder limit,
					// backup folder, or layout follow the config file, and
					// the hints say so.
					dat: store.dat,
					move_source: store.moveSource,
					playlists: store.playlists,
					multi_disc_dirs: store.multiDiscDirs,
					allow_encrypted: store.allowEncrypted,
					max_depth: store.maxDepth ?? undefined,
					keys: store.keys || null,
					// Always explicit: organize ignores the global default policy.
					on_conflict: store.onConflict,
					skip_space_check: store.skipSpaceCheck,
					verify_after: store.verifyAfter,
					input_exclude: splitLines(store.inputExclude) ?? [],
					filter_regex: splitLines(store.filterRegex) ?? [],
					filter_regex_exclude: splitLines(store.filterRegexExclude) ?? [],
					filter_language: splitList(store.filterLanguage) ?? [],
					filter_region: splitList(store.filterRegion) ?? [],
					no_type: splitList(store.noType) ?? [],
					only_type: splitList(store.onlyType) ?? [],
					only_retail: store.onlyRetail,
					// The best-release fields only apply when single and dat are on;
					// gated-off values still send their explicit no-op.
					single: store.single && store.dat,
					prefer_game_regex:
						store.single && store.dat ? (splitLines(store.preferGameRegex) ?? []) : [],
					prefer_verified: store.single && store.dat && store.preferVerified,
					prefer_good: store.single && store.dat && store.preferGood,
					prefer_language:
						store.single && store.dat ? (splitList(store.preferLanguage) ?? []) : [],
					prefer_region:
						store.single && store.dat ? (splitList(store.preferRegion) ?? []) : [],
					prefer_revision:
						store.single && store.dat ? store.preferRevision || "any" : "any",
					prefer_retail: store.single && store.dat && store.preferRetail,
					prefer_parent: store.single && store.dat && store.preferParent,
					prefer_filename_regex: splitLines(store.preferFilenameRegex) ?? [],
					dir_letter: store.dirLetter,
					// Letter folders on: a blank count is the documented
					// default of 1; a blank limit follows the config file.
					dir_letter_count: store.dirLetter ? (store.dirLetterCount ?? 1) : undefined,
					dir_letter_limit: store.dirLetter ? (store.dirLetterLimit ?? undefined) : undefined,
					dir_letter_group: store.dirLetter && store.dirLetterGroup,
					zip_format: store.zipFormat || undefined,
					zip_exclude: store.zipExclude.trim(),
					link_mode: store.linkMode || undefined,
					// Only an explicitly chosen symlink mode can carry the
					// relative flag; Config default leaves both to the config.
					symlink_relative: store.linkMode === "symlink" ? store.symlinkRelative : undefined,
					// [] is the bare-flag form: strip every detected header.
					// "none" matches no header extension, so an empty field
					// strips nothing instead of taking the config value.
					remove_headers:
						store.removeHeaders.trim().toLowerCase() === "all"
							? []
							: (splitList(store.removeHeaders) ?? ["none"]),
					trim_add_padding: store.trimAddPadding,
					patch: splitLines(store.patch) ?? [],
					patch_only: store.patch.trim() !== "" && store.patchOnly,
					clean: store.clean,
					clean_exclude: splitLines(store.cleanExclude) ?? [],
					clean_backup: store.cleanBackup || undefined,
					move_delete_dirs: (store.moveSource && store.moveDeleteDirs) || undefined,
				},
				false,
				taskId,
			),
		chips: () => "TorrentZip · RVZ · CHD · NSZ · CSO · Best release",
	},
];

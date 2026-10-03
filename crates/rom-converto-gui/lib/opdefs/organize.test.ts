import { describe, expect, it, beforeEach, afterEach } from "vitest";
import { readFileSync, writeFileSync } from "node:fs";
import { buildCliCommand, setWindowsQuoting } from "../../composables/useCliEcho";
import { organizeOps } from "./organize";
import type { RunPayload } from "./types";

const ITEM = { id: "item-1", path: "/roms/library", name: "library", size: 0, outExt: "" };

// The POSIX-quote expectations below must not depend on the host OS.
beforeEach(() => setWindowsQuoting(false));

const organizeOp = organizeOps[0]!;

function payloadOf(overrides: Record<string, unknown> = {}): RunPayload {
	const store = organizeOp.useStore() as Record<string, unknown> & { $reset?: () => void };
	store.$reset?.();
	Object.assign(store, overrides);
	return organizeOp.buildArgs(store as never, ITEM, "task-1") as RunPayload;
}

// The default and fully-on GUI payloads, rendered as options and as the
// echoed CLI command. The Rust test
// commands::organize::tests::gui_echo_fixture_round_trips parses this file's
// echo strings with clap and compares the effective options against the
// recorded payload, so the two sides cannot drift. Run vitest with
// UPDATE_ECHO_FIXTURE=1 to rewrite the file after a deliberate change.
const FIXTURE_PATH = new URL("./organize_echo_fixture.json", import.meta.url);

describe("organize echo fixture", () => {
	it("matches the committed echo and payload fixture", () => {
		// Values chosen to differ from the hostile configs the Rust round
		// trip applies, so a dropped flag cannot hide behind the fill.
		const on: Record<string, unknown> = {
			outputDir: "/roms/sorted",
			outputTemplate: "{console}/{basename}.{ext}",
			dat: true,
			moveSource: true,
			playlists: true,
			multiDiscDirs: true,
			allowEncrypted: true,
			maxDepth: 2,
			keys: "~/prod.keys",
			onConflict: "error",
			skipSpaceCheck: true,
			verifyAfter: true,
			inputExclude: "**/*-beta.*\n**/*.nav",
			filterRegex: "(USA|Europe)",
			filterRegexExclude: "\\(Demo\\)",
			filterLanguage: "EN,FR",
			filterRegion: "USA,EUR",
			noType: "demo,beta",
			onlyType: "program",
			onlyRetail: true,
			single: true,
			preferGameRegex: "Rev A",
			preferVerified: true,
			preferGood: true,
			preferLanguage: "EN,FR",
			preferRegion: "USA,EUR",
			preferRevision: "older",
			preferRetail: true,
			preferParent: true,
			preferFilenameRegex: "Redump",
			dirLetter: true,
			dirLetterCount: 2,
			dirLetterLimit: 10,
			dirLetterGroup: true,
			zipFormat: "torrentzip",
			zipExclude: "**/*.zip",
			linkMode: "hardlink",
			symlinkRelative: false,
			removeHeaders: "nes,fds",
			trimAddPadding: true,
			patch: "~/patches",
			patchOnly: true,
			clean: true,
			cleanExclude: "**/*.txt",
			cleanBackup: "~/backup",
			moveDeleteDirs: "never",
		};
		const fixture = {
			default: {
				// The GUI requires an output directory before a run can start,
				// so even the default payload carries one.
				echo: buildCliCommand(payloadOf({ outputDir: "/roms/sorted" })),
				options: payloadOf({ outputDir: "/roms/sorted" }).request.options,
			},
			on: {
				echo: buildCliCommand(payloadOf(on)),
				options: payloadOf(on).request.options,
			},
			// Symlink mode without move: the only shape where
			// symlink_relative travels, and false differs from the hostile
			// config's true.
			symlink: {
				echo: buildCliCommand(
					payloadOf({ outputDir: "/roms/sorted", linkMode: "symlink", symlinkRelative: false }),
				),
				options: payloadOf({
					outputDir: "/roms/sorted",
					linkMode: "symlink",
					symlinkRelative: false,
				}).request.options,
			},
		};
		const rendered = JSON.stringify(fixture, null, 2) + "\n";
		if (process.env.UPDATE_ECHO_FIXTURE) {
			writeFileSync(FIXTURE_PATH, rendered);
			return;
		}
		expect(readFileSync(FIXTURE_PATH, "utf8")).toBe(rendered);
	});
});

// Every field the organize form shows must travel with an explicit value, so
// a config file cannot quietly apply behind a toggle that is off or a field
// that is hidden.
describe("organize buildArgs", () => {
	it("sends false for every off toggle", () => {
		const options = payloadOf().request.options;
		expect(options.dat).toBe(false);
		expect(options.move_source).toBe(false);
		expect(options.playlists).toBe(false);
		expect(options.multi_disc_dirs).toBe(false);
		expect(options.allow_encrypted).toBe(false);
		expect(options.verify_after).toBe(false);
		expect(options.only_retail).toBe(false);
		expect(options.single).toBe(false);
		expect(options.dir_letter).toBe(false);
		expect(options.dir_letter_group).toBe(false);
		// symlink_relative only travels with an explicitly chosen symlink
		// mode; Config default leaves it to the config file.
		expect(options.symlink_relative).toBeUndefined();
		expect(options.trim_add_padding).toBe(false);
		expect(options.patch_only).toBe(false);
		expect(options.clean).toBe(false);
	});

	it("sends explicit no-ops for hidden best-release fields while single is off", () => {
		const options = payloadOf().request.options;
		expect(options.prefer_verified).toBe(false);
		expect(options.prefer_good).toBe(false);
		expect(options.prefer_retail).toBe(false);
		expect(options.prefer_parent).toBe(false);
		expect(options.prefer_game_regex).toEqual([]);
		expect(options.prefer_language).toEqual([]);
		expect(options.prefer_region).toEqual([]);
		expect(options.prefer_revision).toBe("any");
	});

	it("sends the chosen best-release values only when single and dat are on", () => {
		const singleOnly = payloadOf({ single: true }).request.options;
		expect(singleOnly.single).toBe(false);
		expect(singleOnly.prefer_verified).toBe(false);
		expect(singleOnly.prefer_revision).toBe("any");

		const both = payloadOf({
			dat: true,
			single: true,
			preferVerified: true,
			preferRevision: "older",
		}).request.options;
		expect(both.single).toBe(true);
		expect(both.prefer_verified).toBe(true);
		expect(both.prefer_revision).toBe("older");
	});

	it("gates patch_only on the patch field and sends explicit no-op lists", () => {
		const options = payloadOf().request.options;
		expect(options.patch_only).toBe(false);
		expect(options.patch).toEqual([]);
		expect(options.filter_regex).toEqual([]);
		expect(options.input_exclude).toEqual([]);
		expect(options.clean_exclude).toEqual([]);
		expect(options.prefer_filename_regex).toEqual([]);
		// An empty strip-header field strips nothing instead of taking the
		// config value; "all" is the bare-flag form for every header.
		expect(options.remove_headers).toEqual(["none"]);
		expect(payloadOf({ removeHeaders: "all" }).request.options.remove_headers).toEqual([]);
	});

	it("gates the dir-letter numbers on the dirLetter toggle", () => {
		const options = payloadOf({ dirLetter: true, dirLetterCount: 2, dirLetterLimit: 10 }).request.options;
		expect(options.dir_letter_count).toBe(2);
		expect(options.dir_letter_limit).toBe(10);
		// Letter folders on with a blank count sends the documented default.
		const blank = payloadOf({ dirLetter: true }).request.options;
		expect(blank.dir_letter_count).toBe(1);
		expect(blank.dir_letter_limit).toBeUndefined();
		expect(payloadOf().request.options.dir_letter_count).toBeUndefined();
		expect(payloadOf().request.options.dir_letter_limit).toBeUndefined();
	});

	it("splits glob fields by line and keeps commas inside globs", () => {
		const options = payloadOf({
			cleanExclude: "**/*.{txt,pdf}\n**/covers/**",
			inputExclude: "**/*-beta.*\n**/*.nav",
		}).request.options;
		expect(options.clean_exclude).toEqual(["**/*.{txt,pdf}", "**/covers/**"]);
		expect(options.input_exclude).toEqual(["**/*-beta.*", "**/*.nav"]);
	});

	it("echoes the explicit off forms the CLI understands", () => {
		const echo = buildCliCommand(payloadOf());
		expect(echo).toContain("--move=false");
		expect(echo).toContain("--clean=false");
		expect(echo).toContain("--prefer-revision any");
		expect(echo).toContain("--remove-headers=none");
		expect(echo).toContain("--zip-exclude=");
		expect(echo).toContain("--filter-language=");
		expect(echo).toContain("--clean-exclude=");
		// A toggle without a config default stays silent when off.
		expect(echo).not.toContain("--verify-after=false");
		// The relative flag only rides on an explicit symlink mode.
		expect(echo).not.toContain("--symlink-relative");
		const symlink = buildCliCommand(
			payloadOf({ linkMode: "symlink", symlinkRelative: true }),
		);
		expect(symlink).toContain("--link-mode symlink");
		expect(symlink).toContain("--symlink-relative");
	});

	it("echoes patch_only only when a patch is set", () => {
		expect(buildCliCommand(payloadOf())).not.toContain("--patch-only");
		const withPatch = buildCliCommand(payloadOf({ patch: "~/patches", patchOnly: true }));
		expect(withPatch).toContain("--patch-only");
	});
});

describe("windows quoting", () => {
	afterEach(() => setWindowsQuoting(false));

	it("escapes trailing backslashes and inner quotes the MSVC argv way", () => {
		setWindowsQuoting(true);
		const drive = buildCliCommand(payloadOf({ outputDir: "E:\\" }));
		// A lone trailing backslash would escape the closing quote under the
		// MSVC argv rules, so it is doubled inside the quotes.
		expect(drive).toContain('--output-dir "E:\\\\"');
		const quoted = buildCliCommand(payloadOf({ outputTemplate: 'a"b' }));
		expect(quoted).toContain('--output-template "a\\"b"');
	});
});

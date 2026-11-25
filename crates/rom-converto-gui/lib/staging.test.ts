import { beforeEach, describe, expect, it, vi } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { useStaging } from "./staging";
import { runArgs, type OpDef } from "~/lib/opdefs/types";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("~/lib/ipc", () => ({ invoke }));

const def: OpDef = {
	op: "tools",
	console: "nx-merge",
	opLabel: "Tools",
	storeId: "nx-merge",
	useStore: () => ({ output: "", outputDir: "", keys: "", format: "nsp" }),
	command: "cmd_run",
	resultKind: "convert",
	title: "Merge Switch NSP/XCI",
	subtitle: "",
	dropText: "",
	acceptedExts: ["nsp", "xci"],
	browseDirectory: true,
	fields: [],
	outputRows: [],
	actionNote: "",
	deriveOutput: (input) => `${input} (Merged).nsp`,
	buildArgs: (store, item, taskId) => runArgs("nx.merge", item.path, null, {}, false, taskId),
	chips: () => "",
};

beforeEach(() => {
	invoke.mockReset();
	setActivePinia(createPinia());
});

describe("useStaging", () => {
	it("stages a plain file without the dir flag and updates its size reactively", async () => {
		const size = Promise.withResolvers<number>();
		invoke.mockImplementation(async (cmd: string) => {
			if (cmd === "cmd_scan_dir") throw new Error("not a directory");
			if (cmd === "cmd_file_size") return size.promise;
			return null;
		});
		const { staged, add } = useStaging(def);
		await add(["/lib/game.nsp"]);
		expect(staged.value).toHaveLength(1);
		expect(staged.value[0]).toMatchObject({ path: "/lib/game.nsp", name: "game.nsp", dir: false, outExt: "nsp", size: 0 });
		size.resolve(4096);
		await vi.waitFor(() => expect(staged.value[0]!.size).toBe(4096));
	});

	it("remembers the scanned folder on its files, and nothing on files added one by one", async () => {
		invoke.mockImplementation(async (cmd: string, args: { dir?: string }) => {
			if (cmd === "cmd_scan_dir") {
				if (args.dir === "/lib/roms") return ["/lib/roms/a.nsp", "/lib/roms/sub/b.nsp"];
				throw new Error("not a directory");
			}
			return 0;
		});
		const { staged, add } = useStaging(def);
		await add(["/lib/roms", "/lib/single.nsp"]);
		expect(staged.value.map((s) => s.sourceRoot)).toEqual(["/lib/roms", "/lib/roms", undefined]);
	});

	it("stages an empty folder for folder-input ops without asking its size", async () => {
		invoke.mockImplementation(async (cmd: string) => (cmd === "cmd_scan_dir" ? [] : 4096));
		const { staged, add } = useStaging(def);
		await add(["/lib/cdn"]);
		expect(staged.value).toHaveLength(1);
		expect(staged.value[0]!.dir).toBe(true);
		const sizeCalls = invoke.mock.calls.filter(([cmd]) => cmd === "cmd_file_size");
		expect(sizeCalls).toHaveLength(0);
	});

	it("skips a path that is already staged", async () => {
		invoke.mockImplementation(async (cmd: string) => {
			if (cmd === "cmd_scan_dir") throw new Error("not a directory");
			return 1;
		});
		const { staged, add } = useStaging(def);
		await add(["/lib/game.nsp"]);
		await add(["/lib/game.nsp"]);
		expect(staged.value).toHaveLength(1);
	});

	it("stages one folder per game subfolder of a base folder and ignores loose files", async () => {
		const games = { ...def, browseDirectory: false, subfolderItems: () => true };
		invoke.mockImplementation(async (cmd: string) =>
			cmd === "cmd_scan_dir"
				? ["/base/Game A (Merged).nsp", "/base/Game A/base.xci", "/base/Game A/dlc/dlc.nsp", "/base/Game B/update.nsp"]
				: 4096,
		);
		const { staged, add } = useStaging(games);
		await add(["/base/"]);
		expect(staged.value.map((s) => [s.path, s.name, s.dir])).toEqual([
			["/base/Game A", "Game A", true],
			["/base/Game B", "Game B", true],
		]);
		expect(invoke.mock.calls.filter(([cmd]) => cmd === "cmd_file_size")).toHaveLength(0);
	});

	it("does not stage the output directory as a game", async () => {
		const games = {
			...def,
			browseDirectory: false,
			subfolderItems: () => true,
			useStore: () => ({ output: "", outputDir: "/base/merged/", keys: "", format: "nsp" }),
		};
		invoke.mockImplementation(async (cmd: string) =>
			cmd === "cmd_scan_dir" ? ["/base/merged/Game A (Merged).nsp", "/base/Game A/base.nsp"] : 4096,
		);
		const { staged, add } = useStaging(games);
		await add(["/base"]);
		expect(staged.value.map((s) => s.path)).toEqual(["/base/Game A"]);
	});

	it("stages nothing for a dropped folder when the def turns folder scanning off", async () => {
		const folderless = { ...def, scanFolders: false };
		invoke.mockImplementation(async (cmd: string) =>
			cmd === "cmd_scan_dir" ? ["/g/game.m3u", "/g/Disc 1.cue", "/g/Disc 2.cue"] : null,
		);
		const { staged, add } = useStaging(folderless);
		await add(["/g"]);
		expect(staged.value).toHaveLength(0);
	});

	it("stages a plain file under a def that turns folder scanning off", async () => {
		const folderless = { ...def, scanFolders: false };
		invoke.mockImplementation(async (cmd: string) => {
			if (cmd === "cmd_scan_dir") throw new Error("not a directory");
			return 1;
		});
		const { staged, add } = useStaging(folderless);
		await add(["/g/game.m3u"]);
		expect(staged.value).toHaveLength(1);
		expect(staged.value[0]).toMatchObject({ path: "/g/game.m3u", name: "game.m3u", dir: false });
	});
});

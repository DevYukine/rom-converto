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
});

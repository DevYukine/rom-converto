import { describe, expect, it } from "vitest";
import { handlers } from "./ipc-mock";
import { registeredCommands } from "./source-parsers";

describe("ipc-mock", () => {
	it("covers every command the Tauri backend registers", () => {
		const registered = registeredCommands();
		expect(registered.length).toBeGreaterThan(0);
		const uncovered = registered.filter((cmd) => !(cmd in handlers));
		expect(uncovered).toEqual([]);
	});
});

import { describe, expect, it } from "vitest";
import { expandTildePaths } from "./ipc";

describe("expandTildePaths", () => {
	const home = "/home/user";

	it("expands a bare ~", () => {
		expect(expandTildePaths("~", home)).toBe("/home/user");
	});

	it("expands ~/ paths", () => {
		expect(expandTildePaths("~/roms/output", home)).toBe("/home/user/roms/output");
	});

	it("expands ~\\ paths keeping the original separator", () => {
		expect(expandTildePaths("~\\roms\\output", home)).toBe("/home/user\\roms\\output");
	});

	it("expands strings inside nested objects and arrays", () => {
		const input = { paths: ["~/roms/output", "~/roms/input"], nested: { dir: "~" } };
		expect(expandTildePaths(input, home)).toEqual({
			paths: ["/home/user/roms/output", "/home/user/roms/input"],
			nested: { dir: "/home/user" },
		});
	});

	it("leaves non-tilde strings, numbers, and null unchanged", () => {
		expect(expandTildePaths("/roms/output", home)).toBe("/roms/output");
		expect(expandTildePaths(42, home)).toBe(42);
		expect(expandTildePaths(null, home)).toBe(null);
	});

	it("leaves ~user paths unchanged", () => {
		expect(expandTildePaths("~user/x", home)).toBe("~user/x");
	});

	it("passes non-plain objects through untouched", () => {
		const bytes = new Uint8Array([1, 2, 3]);
		expect(expandTildePaths(bytes, home)).toBe(bytes);
		const date = new Date(0);
		expect(expandTildePaths({ when: date }, home)).toEqual({ when: date });
	});
});

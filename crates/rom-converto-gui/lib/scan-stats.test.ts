import { describe, expect, it } from "vitest";
import { createRateMeter, formatElapsed, formatEta, formatRate, relativePath } from "./scan-stats";

describe("relativePath", () => {
	it("strips the scan root across separator styles and case", () => {
		expect(relativePath("D:\\Roms\\cdn\\0004000\\00000000", "d:/roms/cdn")).toBe("0004000/00000000");
		expect(relativePath("/lib/a/b.iso", "/lib/")).toBe("a/b.iso");
	});

	it("falls back to the file name outside the root", () => {
		expect(relativePath("/elsewhere/x.chd", "/lib")).toBe("x.chd");
	});
});

describe("formatRate", () => {
	it("switches units by magnitude", () => {
		expect(formatRate(85.4)).toBe("85 files/s");
		expect(formatRate(2.34)).toBe("2.3 files/s");
		expect(formatRate(0.05)).toBe("20 s/file");
		expect(formatRate(0)).toBe("");
	});
});

describe("formatEta", () => {
	it("rounds coarsely and upward", () => {
		expect(formatEta(4)).toBe("a few seconds left");
		expect(formatEta(41)).toBe("about 50 s left");
		expect(formatEta(61)).toBe("about 2 min left");
		expect(formatEta(3600)).toBe("about 1 h left");
		expect(formatEta(3900)).toBe("about 1 h 5 min left");
		expect(formatEta(Number.NaN)).toBe("");
	});
});

describe("formatElapsed", () => {
	it("drops zero remainders", () => {
		expect(formatElapsed(12_400)).toBe("12 s");
		expect(formatElapsed(120_000)).toBe("2 min");
		expect(formatElapsed(134_000)).toBe("2 min 14 s");
		expect(formatElapsed(3_600_000)).toBe("1 h");
		expect(formatElapsed(3_900_000)).toBe("1 h 5 min");
	});
});

describe("createRateMeter", () => {
	it("ignores samples inside the minimum interval and smooths the rest", () => {
		const m = createRateMeter();
		expect(m.sample(0, 0)).toBe(0);
		expect(m.sample(100, 50)).toBe(0);
		expect(m.sample(1000, 100)).toBe(100);
		expect(m.sample(2000, 120)).toBe(76);
	});

	it("restarts when the counter goes backwards", () => {
		const m = createRateMeter();
		m.sample(0, 0);
		m.sample(1000, 100);
		expect(m.sample(1500, 0)).toBe(0);
		expect(m.sample(2500, 10)).toBe(10);
	});
});

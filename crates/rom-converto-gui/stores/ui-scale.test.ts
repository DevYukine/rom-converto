import { describe, expect, it } from "vitest";
import { scaleFitsWorkArea, scaleForWorkArea } from "~/stores/ui";

describe("scaleForWorkArea", () => {
	it("keeps the preference when it fits", () => {
		expect(scaleForWorkArea(1.5, { width: 1600, height: 900 })).toBe(1.5);
		expect(scaleForWorkArea(0.9, { width: 800, height: 440 })).toBe(0.9);
	});

	it("clamps to the largest step that fits", () => {
		// 200% needs 1600x880; 150% needs 1200x660, which 1440x900 fits.
		expect(scaleForWorkArea(2.0, { width: 1440, height: 900 })).toBe(1.5);
		// 150% needs 660 of height; 130% needs 572, which 600 fits.
		expect(scaleForWorkArea(2.0, { width: 2560, height: 600 })).toBe(1.3);
	});

	it("falls back to the smallest step when nothing fits", () => {
		expect(scaleForWorkArea(2.0, { width: 500, height: 300 })).toBe(0.9);
	});

	it("keeps the preference when no work area is known", () => {
		expect(scaleForWorkArea(2.0, null)).toBe(2.0);
	});
});

describe("scaleFitsWorkArea", () => {
	it("tests the 800x440 minimum against the work area", () => {
		expect(scaleFitsWorkArea(2.0, { width: 1600, height: 880 })).toBe(true);
		expect(scaleFitsWorkArea(2.0, { width: 1600, height: 879 })).toBe(false);
		expect(scaleFitsWorkArea(1.0, null)).toBe(true);
	});
});

import { describe, expect, it } from "vitest";
import { ageRatingDisplayName, contentTypeDisplayName, enumDisplayName, languageDisplayName } from "./display";

describe("enumDisplayName", () => {
	it("splits snake_case words and capitalizes them", () => {
		expect(enumDisplayName("add_on_content")).toBe("Add On Content");
	});

	it("splits PascalCase on lowercase to uppercase boundaries", () => {
		expect(enumDisplayName("AmericanEnglish")).toBe("American English");
	});
});

describe("languageDisplayName", () => {
	it("maps snake_case and PascalCase variants to the same name", () => {
		expect(languageDisplayName("simplified_chinese")).toBe("Chinese (Simplified)");
		expect(languageDisplayName("SimplifiedChinese")).toBe("Chinese (Simplified)");
	});

	it("falls back to enumDisplayName for unknown tags", () => {
		expect(languageDisplayName("klingon_dialect")).toBe("Klingon Dialect");
	});
});

describe("contentTypeDisplayName", () => {
	it("maps add_on_content to DLC", () => {
		expect(contentTypeDisplayName("add_on_content")).toBe("DLC");
	});

	it("falls back to enumDisplayName for unknown types", () => {
		expect(contentTypeDisplayName("mystery_content")).toBe("Mystery Content");
	});
});

describe("ageRatingDisplayName", () => {
	it("maps PegiBbfc to BBFC", () => {
		expect(ageRatingDisplayName("PegiBbfc")).toBe("BBFC");
	});

	it("maps bbfc to BBFC", () => {
		expect(ageRatingDisplayName("bbfc")).toBe("BBFC");
	});

	it("maps pegi_fin to PEGI (Finland)", () => {
		expect(ageRatingDisplayName("pegi_fin")).toBe("PEGI (Finland)");
	});

	it("leaves unknown organizations unchanged", () => {
		expect(ageRatingDisplayName("XYZ")).toBe("XYZ");
	});
});

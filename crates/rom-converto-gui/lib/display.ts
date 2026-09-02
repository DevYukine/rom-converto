export function enumDisplayName(raw: string): string {
	return raw
		.replace(/_/g, " ")
		.replace(/([a-z])([A-Z])/g, "$1 $2")
		.split(/\s+/)
		.filter(Boolean)
		.map((word) => word.charAt(0).toUpperCase() + word.slice(1))
		.join(" ");
}

function normalize(tag: string): string {
	return tag.replace(/[_\s]/g, "").toLowerCase();
}

const LANGUAGE_NAMES: Record<string, string> = {
	japanese: "Japanese",
	english: "English",
	americanenglish: "English (US)",
	britishenglish: "English (UK)",
	french: "French",
	canadianfrench: "French (Canada)",
	german: "German",
	italian: "Italian",
	spanish: "Spanish",
	latinamericanspanish: "Spanish (Latin America)",
	dutch: "Dutch",
	portuguese: "Portuguese",
	brazilianportuguese: "Portuguese (Brazil)",
	russian: "Russian",
	korean: "Korean",
	simplifiedchinese: "Chinese (Simplified)",
	traditionalchinese: "Chinese (Traditional)",
	chinese: "Chinese",
	taiwanesechinese: "Chinese (Taiwan)",
	default: "Default",
};

export function languageDisplayName(tag: string): string {
	return LANGUAGE_NAMES[normalize(tag)] ?? enumDisplayName(tag);
}

const CONTENT_TYPE_NAMES: Record<string, string> = {
	application: "Game",
	patch: "Update",
	addoncontent: "DLC",
	delta: "Delta",
	systemprogram: "System Program",
	systemdata: "System Data",
	systemupdate: "System Update",
	game: "Game",
	update: "Update",
	dlc: "DLC",
	demo: "Demo",
	system: "System",
	unknown: "Unknown",
};

export function contentTypeDisplayName(raw: string): string {
	return CONTENT_TYPE_NAMES[normalize(raw)] ?? enumDisplayName(raw);
}

const AGE_RATING_NAMES: Record<string, string> = {
	cero: "CERO",
	esrb: "ESRB",
	usk: "USK",
	bbfc: "BBFC",
	pegifin: "PEGI (Finland)",
	pegigen: "PEGI",
	pegi: "PEGI",
	pegiprt: "PEGI (Portugal)",
	pegiportugal: "PEGI (Portugal)",
	pegibbfc: "BBFC",
	cob: "COB",
	grb: "GRB",
	gracgcrb: "GRAC",
	cgsrr: "CGSRR",
	gsrmr: "GSRMR",
	classind: "ClassInd",
	russian: "RARS",
	acb: "ACB",
	oflc: "OFLC",
	iarcgeneric: "IARC",
};

export function ageRatingDisplayName(org: string): string {
	return AGE_RATING_NAMES[normalize(org)] ?? org;
}

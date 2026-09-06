import { kindModules } from "./inspect";
import type { InfoResult } from "~/types/info";
import type { InfoKind, InspectView, KindModule } from "./inspect/types";

export * from "./inspect/types";
export { pkgPlatformBadge } from "./inspect/pkg";
export { wupEncryption } from "./inspect/wup";
export { formatBytes } from "./inspect/shared";

/** The kind module for an InfoResult, widened so callers can pass the union. */
export function moduleFor(info: InfoResult): KindModule<InfoKind> {
	// The map is correlated with `kind`; TypeScript can't track that through the union.
	return kindModules[info.kind] as KindModule<InfoKind>;
}

export function buildInspectView(info: InfoResult): InspectView {
	const build = moduleFor(info).build(info);
	const rom = build.rom ?? [];
	return {
		container: build.container ?? [],
		rom,
		innerTitle: build.innerTitle ?? "Inner Files",
		innerFiles: build.innerFiles ?? [],
		hashes: build.hashes ?? [],
		contentType: rom.find((f) => f.label === "Content Type")?.value ?? null,
	};
}

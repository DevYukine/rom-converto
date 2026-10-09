import type { DryRunLine } from "~/lib/dry-run";
import { relativePath } from "~/lib/scan-stats";
import type { OrganizeRow } from "~/types";

// Human labels for `OrganizeRow.action`: plain actions by name, converter
// actions (`dol.compress`, `chd.migrate`, ...) by their suffix. Failed units
// carry the bare `organize` action.
const ACTION_LABELS: Record<string, string> = {
	zip: "Zip", copy: "Copy", move: "Move",
	link: "Link", clean: "Clean", playlist: "Playlist", skip: "", organize: "",
};
const SUFFIX_LABELS: Record<string, string> = {
	migrate: "Migrate", convert: "Convert", decrypt: "Decrypt", compress: "Compress",
};

export function actionLabel(action: string): string {
	return ACTION_LABELS[action] ?? SUFFIX_LABELS[action.slice(action.lastIndexOf(".") + 1)] ?? action;
}

// Plan decisions that replace or move an existing target show as conflicts,
// like the planner's own decision texts ("overwrite", "rewrite (invalid)").
const CONFLICT_DETAIL = /exists|rename|overwrite|rewrite/i;

// Clean rows point at stale files under the output root, not the library, so
// a path outside the roots keeps its full path instead of collapsing to its
// basename.
export function organizeDryRunLines(rows: OrganizeRow[], libraryRoot: string, outputRoot: string): DryRunLine[] {
	return rows.map((row) => {
		const conflict = row.status === "failed" || row.action === "clean" || CONFLICT_DETAIL.test(row.detail ?? "");
		return {
			source: relativePath(row.input, libraryRoot, "path"),
			output: row.output ? relativePath(row.output, outputRoot, "path") : "",
			note: [actionLabel(row.action), row.detail].filter(Boolean).join(" · ") || row.status,
			conflict,
			muted: row.status === "skipped" && !conflict,
		};
	});
}

import type { DryRunLine } from "~/components/modals/DryRunModal.vue";
import type { OrganizeRow } from "~/types";

// Human labels for `OrganizeRow.action`: plain actions by name, converter
// actions (`dol.compress`, `chd.migrate`, ...) by their suffix. Failed units
// carry the bare `organize` action.
const ACTION_LABELS: Record<string, string> = {
	compress: "Compress", zip: "Zip", copy: "Copy", move: "Move",
	link: "Link", clean: "Clean", playlist: "Playlist", skip: "", organize: "",
};
const SUFFIX_LABELS: Record<string, string> = {
	migrate: "Migrate", convert: "Convert", decrypt: "Decrypt", compress: "Compress",
};

export function actionLabel(action: string): string {
	return ACTION_LABELS[action] ?? SUFFIX_LABELS[action.slice(action.lastIndexOf(".") + 1)] ?? action;
}

// Clean rows point at stale files under the output root, not the library, so
// a path outside `root` stays absolute instead of collapsing to its basename.
function underRoot(path: string, root: string): string {
	const p = path.replace(/\\/g, "/");
	const r = root.replace(/\\/g, "/").replace(/\/+$/, "");
	return r && p.toLowerCase().startsWith(`${r.toLowerCase()}/`) ? p.slice(r.length + 1) : path;
}

// Playlists arrive as `playlist` rows, so the plan is the row list alone.
export function organizeDryRunLines(rows: OrganizeRow[], libraryRoot: string, outputRoot: string): DryRunLine[] {
	return rows.map((row) => {
		const conflict = row.status === "failed" || row.action === "clean" || /exists|rename/i.test(row.detail ?? "");
		return {
			source: underRoot(row.input, libraryRoot),
			output: row.output ? underRoot(row.output, outputRoot) : "",
			note: [actionLabel(row.action), row.detail].filter(Boolean).join(" · ") || row.status,
			conflict,
			muted: row.status === "skipped" && !conflict,
		};
	});
}

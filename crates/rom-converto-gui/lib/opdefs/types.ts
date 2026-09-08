import type { ResultKind } from "~/stores/queue";
import type { RunOptions, RunRequest } from "~/types";

// A store field is bound live to a control; the concrete op stores are
// heterogeneous Pinia setup stores, so binding is keyed by field name.
export type OpStore = Record<string, any>;

export interface StagedItem {
	id: string;
	path: string;
	name: string;
	size: number;
	outExt: string;
}

export type FieldKind =
	| "slider"
	| "segmented"
	| "toggle"
	| "kv"
	| "number"
	| "text"
	| "file"
	| "multiselect";

export type KvColor = "t3" | "blue" | "green" | "yellow" | "red";

interface FieldBase {
	kind: FieldKind;
	key: string;
	label: string;
	hint?: string;
	tooltip?: string;
	visible?: (store: OpStore) => boolean;
}

export interface SliderField extends FieldBase {
	kind: "slider";
	min: number;
	max: number;
	formatValue?: (value: number) => string;
}

export interface SegmentedField extends FieldBase {
	kind: "segmented";
	options: { label: string; value: string }[];
	// Runs after the user picks a segment (not on programmatic writes).
	onSet?: (store: OpStore) => void;
}

export interface ToggleField extends FieldBase {
	kind: "toggle";
	description?: string;
	note?: (store: OpStore) => string | false;
	disabled?: (store: OpStore) => boolean;
}

export interface KvField extends FieldBase {
	kind: "kv";
	display: (store: OpStore) => string;
	color?: KvColor;
	onClick?: (store: OpStore) => void;
}

export interface NumberField extends FieldBase {
	kind: "number";
	placeholder?: string;
}

export interface TextField extends FieldBase {
	kind: "text";
	placeholder?: string;
}

export interface FileField extends FieldBase {
	kind: "file";
	filters?: { name: string; extensions: string[] }[];
	display: (store: OpStore) => string;
	// Overrides the clickable-blue value color, e.g. found/missing state.
	color?: (store: OpStore) => KvColor | undefined;
}

// Store value is a string[]; selection order is preserved and doubles as
// priority. Default [] means "auto".
export interface MultiselectField extends FieldBase {
	kind: "multiselect";
	options: { value: string; label: string }[];
	max?: number;
	placeholder?: string;
}

export type FieldDef =
	| SliderField
	| SegmentedField
	| ToggleField
	| KvField
	| NumberField
	| TextField
	| FileField
	| MultiselectField;

export interface OutputRow {
	kind: "directory" | "template" | "text" | "report" | "save";
	label: string;
	display: (store: OpStore) => string;
	set?: (store: OpStore, value: string) => void;
	color?: "t3" | "blue" | "green" | "yellow";
	tooltip?: string;
	// kind "save" only: save-dialog filters and suggested filename.
	filters?: { name: string; extensions: string[] }[];
	defaultPath?: string;
}

export interface OpDef {
	op: string;
	console: string;
	opLabel: string;
	storeId: string;
	useStore: () => OpStore;
	command: string | ((store: OpStore) => string);
	resultKind: ResultKind;

	title: string;
	subtitle: string;
	dropText: string;
	acceptedExts: string[];
	browseFilters?: { name: string; extensions: string[] }[];
	defaultOutputDir?: string;
	singleInput?: boolean;
	// Input is a directory; DropZone offers a folder-picker instead of a file dialog.
	browseDirectory?: boolean;
	// Input can be a file or a directory; DropZone offers both pickers.
	browseAlsoDirectory?: boolean;

	// Fixed progress/cancel key for commands that hardcode one (cue, hash,
	// dat, cdn). When absent each job gets a unique `job-<uuid>` key.
	progressKey?: string | ((store: OpStore) => string);

	fields: FieldDef[];
	note?: string;
	// Prominent callout shown between the page head and the drop zone.
	warning?: string;
	outputRows: OutputRow[];

	showConflict?: boolean;
	renameDisabled?: boolean;
	showVerify?: boolean;
	verifyLabel?: string;
	showDryRun?: boolean;
	actionNote: string;

	// Runs after new items are staged (e.g. to adapt defaults to the input kind).
	onStaged?: (store: OpStore, items: StagedItem[]) => void;

	// Output path shown in the staged-row meta and the dry-run plan.
	deriveOutput?: (input: string, store: OpStore) => string;
	buildArgs: (store: OpStore, item: StagedItem, taskId: string) => RunPayload;
	// When set, all staged items build a single spec instead of one per item
	// (e.g. merge, which combines every dropped file into one output).
	buildArgsAll?: (store: OpStore, items: StagedItem[], taskId: string) => RunPayload;
	chips: (store: OpStore) => string;
}

// Ops that write a whole directory instead of deriving a single output
// file, so they have no template/report row.
export function directoryOutputRows(tooltip: string): OutputRow[] {
	return [
		{
			kind: "directory",
			label: "Directory",
			display: (s) => s.outputDir || "same as source",
			set: (s, v) => { s.outputDir = v; },
			tooltip,
		},
	];
}

// One `cmd_run` payload: the RunRequest the library runner deserialises, plus
// the task id and report flag the shim reads beside it. Only the options an op
// actually sets travel; the runner defaults the rest.
// A type alias, not an interface: it has to stay assignable to the
// `Record<string, unknown>` the Tauri `invoke` shim takes.
export type RunPayload = {
	taskId: string;
	report: boolean;
	// Read by the queue, which writes one report per finished group.
	reportFile: string | null;
	request: Omit<RunRequest, "options"> & { options: Partial<RunOptions> };
};

// Option keys are the runner's own snake_case names; unknown keys are rejected
// by the backend.
export function runArgs(
	operation: string,
	input: string | null,
	output: string | null,
	options: Partial<RunOptions>,
	dryRun: boolean,
	taskId: string,
	report: string | null = null,
): RunPayload {
	return {
		taskId,
		report: !!report,
		reportFile: report,
		request: {
			schema: null,
			operation,
			input,
			output,
			config: null,
			preset: null,
			options,
			dry_run: dryRun,
		},
	};
}

// One path out of the RunRequest inside a `cmd_run` payload, for job names,
// plan lines and result rows.
export function requestPath(args: RunPayload, field: "input" | "output"): string {
	return args.request[field] ?? "";
}

// The output/safety tail every write op puts in its RunOptions. `verify_after`
// travels only for the ops that show the toggle, which are exactly the stores
// carrying the field.
export function commonOptions(store: OpStore): Partial<RunOptions> {
	return {
		on_conflict: store.onConflict,
		skip_space_check: store.skipSpaceCheck,
		output_template: store.outputTemplate || null,
		...("verifyAfter" in store ? { verify_after: store.verifyAfter } : {}),
	};
}

// Turns a payload into its dry-run twin.
export function dryRunArgs(args: RunPayload): RunPayload {
	return { ...args, request: { ...args.request, dry_run: true } };
}

export function templateIsActive(store: OpStore): boolean {
	return typeof store.outputTemplate === "string" && store.outputTemplate.length > 0;
}

export function opCommand(def: OpDef, store: OpStore): string {
	return typeof def.command === "function" ? def.command(store) : def.command;
}

export function opProgressKey(def: OpDef, store: OpStore): string | undefined {
	return typeof def.progressKey === "function" ? def.progressKey(store) : def.progressKey;
}

// Mirrors default_candidate_paths() in rom-converto-lib nintendo/nx/keys.rs.
export const NX_KEYS_AUTO = "auto (~/.switch/prod.keys)";
export const NX_KEYS_TOOLTIP =
	"Path to prod.keys, used to decrypt Switch content. When unset, the app looks in ~/.switch (the same location nsz uses), then next to the rom-converto executable.";

export function recursiveFields(): FieldDef[] {
	return [
		{
			kind: "toggle",
			key: "recursive",
			label: "Recursive",
			description: "Scan the dropped folder and process every file inside it",
			tooltip:
				"Processes every matching file inside a dropped folder, skipping junk files, instead of only the folder itself.",
		},
		{
			kind: "number",
			key: "maxDepth",
			label: "Max depth (optional)",
			placeholder: "Unlimited",
			visible: (s) => s.recursive !== false,
			tooltip: "How many folder levels deep the scan goes. Empty means unlimited.",
		},
	];
}

const registry = new Map<string, Map<string, OpDef>>();

export function registerOps(defs: OpDef[]): void {
	for (const def of defs) {
		let consoles = registry.get(def.op);
		if (!consoles) {
			consoles = new Map();
			registry.set(def.op, consoles);
		}
		consoles.set(def.console, def);
	}
}

export function allOpDefs(): OpDef[] {
	return [...registry.values()].flatMap((consoles) => [...consoles.values()]);
}

export function opDef(op: string, console: string): OpDef | undefined {
	return registry.get(op)?.get(console);
}

export function opConsoles(op: string): string[] {
	return [...(registry.get(op)?.keys() ?? [])];
}

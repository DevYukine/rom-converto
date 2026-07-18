import type { ResultKind } from "~/stores/queue";

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
	color?: "t3" | "blue" | "green" | "yellow";
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
	buildArgs: (store: OpStore, item: StagedItem, taskId: string) => Record<string, unknown>;
	chips: (store: OpStore) => string;
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

export function recursiveFields(): FieldDef[] {
	return [
		{
			kind: "toggle",
			key: "recursive",
			label: "Recursive",
			description: "Scan the dropped folder and process every file inside it",
		},
		{
			kind: "number",
			key: "maxDepth",
			label: "Max depth (optional)",
			placeholder: "Unlimited",
			visible: (s) => s.recursive !== false,
		},
	];
}

const registry = new Map<string, Map<string, OpDef>>();

export function registerOp(op: string, defs: Record<string, OpDef>): void {
	let consoles = registry.get(op);
	if (!consoles) {
		consoles = new Map();
		registry.set(op, consoles);
	}
	for (const [console, def] of Object.entries(defs)) consoles.set(console, def);
}

export function opDef(op: string, console: string): OpDef | undefined {
	return registry.get(op)?.get(console);
}

export function opConsoles(op: string): string[] {
	return [...(registry.get(op)?.keys() ?? [])];
}

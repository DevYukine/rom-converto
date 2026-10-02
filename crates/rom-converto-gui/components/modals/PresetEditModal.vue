<script setup lang="ts">
import ModalShell from "~/components/modals/ModalShell.vue";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";
import { boundedNumber } from "~/lib/fields";
import ToggleSwitch from "~/components/ui/ToggleSwitch.vue";
import { useConfigStore } from "~/stores/config";
import type { Preset, PresetFormat } from "~/types";

const props = defineProps<{
	name: string;
	preset: Preset;
}>();

const emit = defineEmits<{ close: [] }>();

const store = useConfigStore();

type FieldKind = "number" | "text" | "conflict" | "list" | "lines" | "select" | "bool";
interface FieldSpec {
	key: string;
	label: string;
	kind: FieldKind;
}

const CONFLICT_OPTIONS = [
	{ label: "Error", value: "error" },
	{ label: "Overwrite", value: "overwrite" },
	{ label: "Skip", value: "skip" },
	{ label: "Rename", value: "rename" },
	{ label: "Overwrite if invalid", value: "overwrite-invalid" },
];

// Enum keys are selects so a typo cannot save an invalid value; the empty
// value removes the key so the config default applies.
const SELECT_OPTIONS: Record<string, { label: string; value: string }[]> = {
	zip_format: [
		{ label: "TorrentZip", value: "torrentzip" },
		{ label: "RVZSTD", value: "rvzstd" },
	],
	link_mode: [
		{ label: "Hardlink", value: "hardlink" },
		{ label: "Symlink", value: "symlink" },
		{ label: "Reflink", value: "reflink" },
	],
	move_delete_dirs: [
		{ label: "Never", value: "never" },
		{ label: "Auto", value: "auto" },
		{ label: "Always", value: "always" },
	],
	prefer_revision: [
		{ label: "Any", value: "any" },
		{ label: "Older", value: "older" },
		{ label: "Newer", value: "newer" },
	],
};

// Inclusive bounds for number fields, matching the CLI's value ranges.
const NUMBER_BOUNDS: Record<string, { min?: number; max?: number }> = {
	dir_letter_count: { min: 1, max: 26 },
	dir_letter_limit: { min: 1 },
};

const DISC_FIELDS: FieldSpec[] = [
	{ key: "level", label: "Level", kind: "number" },
	{ key: "chunk_size", label: "Chunk size", kind: "number" },
	{ key: "on_conflict", label: "On conflict", kind: "conflict" },
	{ key: "output_dir", label: "Output dir", kind: "text" },
	{ key: "report", label: "Report", kind: "text" },
];

const FORMAT_SCHEMA: Record<PresetFormat, FieldSpec[]> = {
	dol: DISC_FIELDS,
	rvl: DISC_FIELDS,
	nx: [
		{ key: "level", label: "Level", kind: "number" },
		{ key: "mode", label: "Mode", kind: "text" },
		{ key: "block_size_exp", label: "Block size exp", kind: "number" },
		{ key: "on_conflict", label: "On conflict", kind: "conflict" },
		{ key: "output_dir", label: "Output dir", kind: "text" },
		{ key: "report", label: "Report", kind: "text" },
	],
	chd: [
		{ key: "hunk_size", label: "Hunk size", kind: "number" },
		{ key: "codecs", label: "Codecs", kind: "list" },
		{ key: "level", label: "Level", kind: "number" },
		{ key: "on_conflict", label: "On conflict", kind: "conflict" },
		{ key: "output_dir", label: "Output dir", kind: "text" },
		{ key: "report", label: "Report", kind: "text" },
	],
	cso: [
		{ key: "block_size", label: "Block size", kind: "number" },
		{ key: "on_conflict", label: "On conflict", kind: "conflict" },
		{ key: "output_dir", label: "Output dir", kind: "text" },
		{ key: "report", label: "Report", kind: "text" },
	],
	wup: [
		{ key: "level", label: "Level", kind: "number" },
		{ key: "on_conflict", label: "On conflict", kind: "conflict" },
	],
	dat: [
		{ key: "api_base", label: "API base", kind: "text" },
		{ key: "report", label: "Report", kind: "text" },
		{ key: "input_checksum_min", label: "Checksum floor", kind: "text" },
		{ key: "input_checksum_max", label: "Checksum ceiling", kind: "text" },
	],
	organize: [
		{ key: "output_dir", label: "Output dir", kind: "text" },
		{ key: "output_template", label: "Layout", kind: "text" },
		{ key: "on_conflict", label: "On conflict", kind: "conflict" },
		{ key: "report", label: "Report", kind: "text" },
		{ key: "dat", label: "Rename with DAT (online)", kind: "bool" },
		{ key: "move_source", label: "Move files", kind: "bool" },
		{ key: "playlists", label: "Write .m3u playlists", kind: "bool" },
		{ key: "filter_regex", label: "Name matches (regex)", kind: "lines" },
		{ key: "filter_regex_exclude", label: "Name excludes (regex)", kind: "lines" },
		{ key: "filter_language", label: "Languages", kind: "list" },
		{ key: "filter_region", label: "Regions", kind: "list" },
		{ key: "no_type", label: "Exclude types", kind: "list" },
		{ key: "only_type", label: "Only types", kind: "list" },
		{ key: "only_retail", label: "Retail only", kind: "bool" },
		{ key: "single", label: "One game per set", kind: "bool" },
		{ key: "prefer_region", label: "Region priority", kind: "list" },
		{ key: "prefer_language", label: "Language priority", kind: "list" },
		{ key: "prefer_revision", label: "Revision", kind: "select" },
		{ key: "prefer_retail", label: "Prefer retail", kind: "bool" },
		{ key: "prefer_parent", label: "Prefer parent", kind: "bool" },
		{ key: "prefer_verified", label: "Prefer verified", kind: "bool" },
		{ key: "prefer_good", label: "Prefer good", kind: "bool" },
		{ key: "prefer_game_regex", label: "Prefer DAT names matching", kind: "lines" },
		{ key: "prefer_filename_regex", label: "Prefer file names matching", kind: "lines" },
		{ key: "dir_letter", label: "First-letter folders", kind: "bool" },
		{ key: "dir_letter_count", label: "Characters per folder", kind: "number" },
		{ key: "dir_letter_limit", label: "Folder limit", kind: "number" },
		{ key: "dir_letter_group", label: "Range grouping", kind: "bool" },
		{ key: "zip_format", label: "Zip format", kind: "select" },
		{ key: "zip_exclude", label: "Don't zip matching", kind: "text" },
		{ key: "link_mode", label: "Link mode", kind: "select" },
		{ key: "symlink_relative", label: "Relative symlinks", kind: "bool" },
		{ key: "remove_headers", label: "Strip headers", kind: "list" },
		{ key: "trim_add_padding", label: "Re-pad trimmed dumps", kind: "bool" },
		{ key: "clean", label: "Remove stale files", kind: "bool" },
		{ key: "clean_exclude", label: "Keep files matching", kind: "lines" },
		{ key: "clean_backup", label: "Clean backup folder", kind: "text" },
		{ key: "move_delete_dirs", label: "Empty folders", kind: "select" },
	],
};

const FORMAT_LABELS: Record<PresetFormat, string> = {
	dol: "GameCube (dol)",
	rvl: "Wii (rvl)",
	nx: "Switch (nx)",
	chd: "CHD",
	cso: "CSO/ZSO",
	wup: "Wii U (wup)",
	dat: "DAT",
	organize: "Organize",
};

// props.preset is a reactive store proxy (and so are its nested objects);
// a JSON round-trip deep-clones it to a plain, editable draft.
const draft = reactive<Preset>(JSON.parse(JSON.stringify(props.preset)) as Preset);

const sections = computed(() =>
	(Object.keys(draft) as PresetFormat[]).filter((f) => draft[f]),
);

function cell(format: PresetFormat, key: string) {
	const value = (draft[format] as Record<string, unknown> | null | undefined)?.[key] ?? null;
	return key === "report" && value === false ? null : value;
}

function setCell(format: PresetFormat, key: string, value: unknown) {
	const section = draft[format] as Record<string, unknown> | null | undefined;
	if (section) section[key] = value === "" ? null : value;
}

function onTextInput(format: PresetFormat, key: string, raw: string) {
	setCell(format, key, raw === "" ? null : raw);
}

function listText(format: PresetFormat, key: string): string {
	const value = cell(format, key);
	if (!Array.isArray(value)) return "";
	// An empty strip-header list is the bare-flag form: strip everything.
	return value.length === 0 && key === "remove_headers" ? "all" : value.join(", ");
}

function onListInput(format: PresetFormat, key: string, raw: string) {
	if (key === "remove_headers" && raw.trim().toLowerCase() === "all") {
		setCell(format, key, []);
		return;
	}
	const values = raw
		.split(",")
		.map((v) => v.trim())
		.filter(Boolean);
	setCell(format, key, values.length ? values : null);
}

// Regex and path values may contain commas, so they are one entry per line.
function linesText(format: PresetFormat, key: string): string {
	const value = cell(format, key);
	return Array.isArray(value) ? value.join("\n") : "";
}

function onLinesInput(format: PresetFormat, key: string, raw: string) {
	const values = raw
		.split(/\r?\n/)
		.map((v) => v.trim())
		.filter(Boolean);
	setCell(format, key, values.length ? values : null);
}

const saving = ref(false);
const error = ref("");

async function save() {
	saving.value = true;
	error.value = "";
	try {
		await store.savePreset(props.name, draft);
		emit("close");
	} catch (e) {
		error.value = String(e);
	} finally {
		saving.value = false;
	}
}
</script>

<template>
	<ModalShell :title="`Edit ${name}`" :width="480" @close="emit('close')">
		<div class="rc-sections">
			<div v-for="format in sections" :key="format" class="rc-section">
				<span class="rc-section__title">{{ FORMAT_LABELS[format] }}</span>
				<div v-for="field in FORMAT_SCHEMA[format]" :key="field.key" class="rc-field" :class="{ 'rc-field--stacked': ['text', 'list', 'lines'].includes(field.kind), 'rc-field--bool': field.kind === 'bool' }">
					<span v-if="field.kind !== 'bool'" class="rc-field__label">{{ field.label }}</span>
					<div v-if="field.kind === 'conflict' || field.kind === 'select'" class="rc-select">
						<select
							v-if="field.kind === 'conflict'"
							class="rc-input"
							:value="cell(format, field.key) ?? ''"
							@change="setCell(format, field.key, ($event.target as HTMLSelectElement).value)"
						>
							<option value="">Default</option>
							<option v-for="opt in CONFLICT_OPTIONS" :key="opt.value" :value="opt.value">
								{{ opt.label }}
							</option>
						</select>
						<select
							v-else
							class="rc-input"
							:value="cell(format, field.key) ?? ''"
							@change="setCell(format, field.key, ($event.target as HTMLSelectElement).value || null)"
						>
							<option value="">Default</option>
							<option v-for="opt in SELECT_OPTIONS[field.key]" :key="opt.value" :value="opt.value">
								{{ opt.label }}
							</option>
						</select>
						<svg class="rc-select__icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true">
							<path d="m6 9 6 6 6-6" />
						</svg>
					</div>
					<input
						v-else-if="field.kind === 'number'"
						type="number"
						placeholder="Default"
						class="rc-input rc-input--mono"
						:min="NUMBER_BOUNDS[field.key]?.min"
						:max="NUMBER_BOUNDS[field.key]?.max"
						:value="cell(format, field.key) ?? ''"
						@input="setCell(format, field.key, boundedNumber($event, NUMBER_BOUNDS[field.key] ?? {}))"
					/>
					<textarea
						v-else-if="field.kind === 'lines'"
						class="rc-input rc-input--mono"
						placeholder="One per line"
						:value="linesText(format, field.key)"
						@input="onLinesInput(format, field.key, ($event.target as HTMLTextAreaElement).value)"
					/>
					<input
						v-else-if="field.kind === 'list'"
						type="text"
						class="rc-input rc-input--mono"
						:placeholder="field.key === 'remove_headers' ? 'Comma-separated, or all' : 'Comma-separated'"
						:value="listText(format, field.key)"
						@input="onListInput(format, field.key, ($event.target as HTMLInputElement).value)"
					/>
					<ToggleSwitch
						v-else-if="field.kind === 'bool'"
						:label="field.label"
						:model-value="cell(format, field.key) === true"
						@update:model-value="setCell(format, field.key, $event)"
					/>
					<input
						v-else
						type="text"
						class="rc-input rc-input--mono"
						:value="cell(format, field.key) ?? ''"
						@input="onTextInput(format, field.key, ($event.target as HTMLInputElement).value)"
					/>
				</div>
			</div>
			<p v-if="sections.length === 0" class="rc-empty">This preset has no format sections.</p>
			<p v-if="error" class="rc-error">{{ error }}</p>
		</div>

		<template #footer>
			<div class="rc-footer-actions">
				<PrimaryButton variant="outlined" @click="emit('close')">Cancel</PrimaryButton>
				<PrimaryButton :disabled="saving" @click="save">{{ saving ? "Saving…" : "Save" }}</PrimaryButton>
			</div>
		</template>
	</ModalShell>
</template>

<style scoped>
.rc-sections {
	display: flex;
	flex-direction: column;
	gap: 16px;
}

.rc-section {
	display: flex;
	flex-direction: column;
}

.rc-section__title {
	margin-bottom: 8px;
	font-size: var(--fs-lg);
	font-weight: 600;
	color: var(--t1);
}

.rc-field {
	display: flex;
	flex-wrap: wrap;
	align-items: center;
	gap: 6px 16px;
	min-height: 32px;
	padding: 6px 0;
}

.rc-field + .rc-field {
	border-top: 1px solid var(--a06);
}

.rc-field__label {
	flex: 1 1 auto;
	min-width: 0;
	font-size: var(--fs-md);
	color: var(--t2);
}

.rc-field--stacked {
	flex-direction: column;
	align-items: stretch;
}

.rc-field--stacked .rc-input {
	width: 100%;
}

.rc-field input[type="number"] {
	width: 112px;
	flex: none;
	text-align: right;
}

.rc-field textarea {
	min-height: 64px;
	resize: vertical;
}

.rc-field--bool {
	padding: 0;
}

.rc-field--bool :deep(.rc-toggle-row) {
	width: 100%;
}

.rc-select {
	position: relative;
	flex: 0 1 200px;
	min-width: 0;
	max-width: 60%;
}

.rc-select select {
	appearance: none;
	width: 100%;
	padding-right: 28px;
}

.rc-select__icon {
	position: absolute;
	right: 9px;
	top: 50%;
	transform: translateY(-50%);
	width: 12px;
	height: 12px;
	color: var(--t5);
	pointer-events: none;
}

.rc-empty {
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	color: var(--t4);
	text-wrap: pretty;
}

.rc-error {
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	color: var(--red);
	overflow-wrap: anywhere;
}

.rc-footer-actions {
	margin-left: auto;
	display: flex;
	gap: 10px;
}
</style>

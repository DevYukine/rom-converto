<script setup lang="ts">
import ModalShell from "~/components/modals/ModalShell.vue";
import { useConfigStore } from "~/stores/config";
import type { Preset, PresetFormat } from "~/types/config";

const props = defineProps<{
	name: string;
	preset: Preset;
}>();

const emit = defineEmits<{ close: [] }>();

const store = useConfigStore();

type FieldKind = "number" | "text" | "conflict" | "list";
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
};

const FORMAT_LABELS: Record<PresetFormat, string> = {
	dol: "GameCube (dol)",
	rvl: "Wii (rvl)",
	nx: "Switch (nx)",
	chd: "CHD",
	cso: "CSO/ZSO",
	wup: "Wii U (wup)",
	dat: "DAT",
};

// props.preset is a reactive store proxy (and so are its nested objects);
// a JSON round-trip deep-clones it to a plain, editable draft.
const draft = reactive<Preset>(JSON.parse(JSON.stringify(props.preset)) as Preset);

const sections = computed(() =>
	(Object.keys(draft) as PresetFormat[]).filter((f) => draft[f]),
);

function cell(format: PresetFormat, key: string) {
	return (draft[format] as Record<string, unknown> | null | undefined)?.[key] ?? null;
}

function setCell(format: PresetFormat, key: string, value: unknown) {
	const section = draft[format] as Record<string, unknown> | null | undefined;
	if (section) section[key] = value;
}

function onNumberInput(format: PresetFormat, key: string, raw: string) {
	setCell(format, key, raw === "" ? null : Number(raw));
}

function onTextInput(format: PresetFormat, key: string, raw: string) {
	setCell(format, key, raw === "" ? null : raw);
}

function listText(format: PresetFormat, key: string): string {
	const value = cell(format, key);
	return Array.isArray(value) ? value.join(", ") : "";
}

function onListInput(format: PresetFormat, key: string, raw: string) {
	const values = raw
		.split(",")
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
				<div v-for="field in FORMAT_SCHEMA[format]" :key="field.key" class="rc-field">
					<span class="rc-field__label">{{ field.label }}</span>
					<select
						v-if="field.kind === 'conflict'"
						class="rc-input"
						:value="cell(format, field.key) ?? 'overwrite'"
						@change="setCell(format, field.key, ($event.target as HTMLSelectElement).value)"
					>
						<option v-for="opt in CONFLICT_OPTIONS" :key="opt.value" :value="opt.value">
							{{ opt.label }}
						</option>
					</select>
					<input
						v-else-if="field.kind === 'number'"
						type="number"
						class="rc-input"
						:value="cell(format, field.key) ?? ''"
						@input="onNumberInput(format, field.key, ($event.target as HTMLInputElement).value)"
					/>
					<input
						v-else-if="field.kind === 'list'"
						type="text"
						class="rc-input"
						placeholder="comma-separated"
						:value="listText(format, field.key)"
						@input="onListInput(format, field.key, ($event.target as HTMLInputElement).value)"
					/>
					<input
						v-else
						type="text"
						class="rc-input"
						:value="cell(format, field.key) ?? ''"
						@input="onTextInput(format, field.key, ($event.target as HTMLInputElement).value)"
					/>
				</div>
			</div>
			<p v-if="sections.length === 0" class="rc-empty">This preset has no format sections.</p>
			<p v-if="error" class="rc-error">{{ error }}</p>
		</div>

		<template #footer>
			<div class="rc-spacer" />
			<button type="button" class="rc-outlined" @click="emit('close')">Cancel</button>
			<PrimaryButton :disabled="saving" @click="save">{{ saving ? "Saving…" : "Save" }}</PrimaryButton>
		</template>
	</ModalShell>
</template>

<style scoped>
.rc-sections {
	display: flex;
	flex-direction: column;
	gap: 14px;
}

.rc-section {
	display: flex;
	flex-direction: column;
	gap: 6px;
}

.rc-section__title {
	font-size: 10.5px;
	font-weight: 700;
	text-transform: uppercase;
	letter-spacing: 0.8px;
	color: var(--t4);
}

.rc-field {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 10px;
}

.rc-field__label {
	font-size: 12px;
	color: var(--t2);
}

.rc-input {
	width: 200px;
	font-family: ui-monospace, monospace;
	font-size: 11.5px;
	color: var(--t1);
	background: var(--bg2);
	border: 1px solid var(--a14);
	border-radius: 6px;
	padding: 4px 8px;
}

.rc-empty {
	font-size: 12px;
	color: var(--t4);
}

.rc-error {
	font-size: 12px;
	color: var(--red);
}

.rc-spacer {
	flex: 1;
}

.rc-outlined {
	background: none;
	border: 1px solid var(--a18);
	color: var(--t3);
	border-radius: 8px;
	padding: 6px 16px;
	font-size: 12.5px;
	cursor: pointer;
}

.rc-outlined:hover {
	border-color: var(--a40);
}
</style>

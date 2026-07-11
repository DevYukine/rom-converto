<script setup lang="ts">
import { computed, ref } from "vue";
import { open, save } from "~/lib/ipc";
import { useConfigStore } from "~/stores/config";
import { useStaging } from "~/lib/staging";
import { buildCliCommand } from "~/composables/useCliEcho";
import ConfigCard from "~/components/ui/ConfigCard.vue";
import LevelSlider from "~/components/ui/LevelSlider.vue";
import Segmented from "~/components/ui/Segmented.vue";
import ToggleSwitch from "~/components/ui/ToggleSwitch.vue";
import KvRow from "~/components/ui/KvRow.vue";
import CliChip from "~/components/ui/CliChip.vue";
import ConflictPopover from "~/components/modals/ConflictPopover.vue";
import DirectoryPickerModal from "~/components/modals/DirectoryPickerModal.vue";
import TemplateEditorModal from "~/components/modals/TemplateEditorModal.vue";
import DropZone from "~/components/op/DropZone.vue";
import StagedList from "~/components/op/StagedList.vue";
import ActionRow from "~/components/op/ActionRow.vue";
import VerifyResultsCard from "~/components/op/VerifyResultsCard.vue";
import HashResultsCard from "~/components/op/HashResultsCard.vue";
import { opCommand, opProgressKey } from "~/lib/opdefs/types";
import type { FieldDef, OpDef, OutputRow, StagedItem } from "~/lib/opdefs/types";

const props = defineProps<{ def: OpDef }>();

const store = props.def.useStore();
const config = useConfigStore();
const { staged, add, remove, clear } = useStaging(props.def);
const { show: showToast } = useToast();

const presetTag = computed(() =>
	props.def.op === "compress" && config.activePreset ? `from ${config.activePreset}` : "",
);

const cli = computed(() => {
	const sample: StagedItem = staged.value[0] ?? { id: "", path: "", name: "", size: 0, outExt: "" };
	const taskId = opProgressKey(props.def, store) ?? "job";
	return buildCliCommand(opCommand(props.def, store), props.def.buildArgs(store, sample, taskId), props.def.console);
});

const stagedLabel = computed(() =>
	props.def.resultKind === "verify"
		? `Staged: ${staged.value.length} files`
		: `Staged: ${staged.value.length} files, not queued yet`,
);

const optionsTitle = computed(() => (props.def.op === "compress" ? "Compression" : "Options"));
const showOptions = computed(() => props.def.fields.length > 0 || !!props.def.note);

const has = (key: string) => key in store;
const showConflict = computed(() => props.def.showConflict !== false && has("onConflict"));
const showVerify = computed(() => !!props.def.showVerify && has("verifyAfter"));
const showSkip = computed(() => has("skipSpaceCheck"));
const showSafety = computed(() => showConflict.value || showVerify.value || showSkip.value);

function visible(field: FieldDef): boolean {
	return field.visible ? field.visible(store) : true;
}

function toNumber(e: Event): number | null {
	const v = (e.target as HTMLInputElement).value;
	return v === "" ? null : Number(v);
}

async function pickFile(field: FieldDef & { filters?: { name: string; extensions: string[] }[] }) {
	const picked = await open({ multiple: false, filters: field.filters });
	if (typeof picked === "string") store[field.key] = picked;
}

const dirRow = ref<OutputRow | null>(null);
const tmplRow = ref<OutputRow | null>(null);

async function openRow(row: OutputRow) {
	if (row.kind === "directory") dirRow.value = row;
	else if (row.kind === "template") tmplRow.value = row;
	else if (row.kind === "report") {
		const picked = await save({ filters: [{ name: "Report", extensions: ["csv", "json", "html"] }] });
		if (typeof picked === "string") row.set?.(store, picked);
	} else if (row.kind === "save") {
		const picked = await save({ filters: row.filters, defaultPath: row.defaultPath });
		if (typeof picked === "string") row.set?.(store, picked);
	}
}

function setDir(value: string) {
	dirRow.value?.set?.(store, value);
}

function setTmpl(value: string) {
	tmplRow.value?.set?.(store, value);
}

function copied() {
	showToast("Copied");
}
</script>

<template>
	<div class="rc-page">
		<div class="rc-head">
			<div class="rc-head__text">
				<h1 class="rc-head__title">{{ def.title }}</h1>
				<p class="rc-head__subtitle">{{ def.subtitle }}</p>
			</div>
			<CliChip :command="cli" @copy="copied" />
		</div>

		<DropZone
			:drop-text="def.dropText"
			:filters="def.browseFilters"
			:multiple="!def.singleInput"
			:directory="def.browseDirectory"
			:also-directory="def.browseAlsoDirectory"
			@add="add"
		/>

		<StagedList
			v-if="staged.length"
			:items="staged"
			:label="stagedLabel"
			:console-name="def.console"
			@remove="remove"
			@clear="clear"
		/>

		<div class="rc-grid">
			<ConfigCard v-if="showOptions" :title="optionsTitle">
				<template v-if="presetTag" #head-tag>
					<span class="rc-preset-tag">{{ presetTag }}</span>
				</template>
				<template v-for="field in def.fields" :key="field.key">
					<template v-if="visible(field)">
						<LevelSlider
							v-if="field.kind === 'slider'"
							:model-value="store[field.key]"
							:min="field.min"
							:max="field.max"
							:label="field.label"
							:hint="field.hint"
							:format-value="field.formatValue"
							@update:model-value="store[field.key] = $event"
						/>
						<div v-else-if="field.kind === 'segmented'" class="rc-field">
							<Segmented
								:model-value="store[field.key]"
								:options="field.options"
								:label="field.label"
								@update:model-value="
									store[field.key] = $event;
									field.onSet?.(store);
								"
							/>
							<p v-if="field.hint" class="rc-field__note">{{ field.hint }}</p>
						</div>
						<div v-else-if="field.kind === 'toggle'" class="rc-field">
							<ToggleSwitch
								:model-value="store[field.key]"
								:label="field.label"
								:description="field.description"
								:disabled="field.disabled ? field.disabled(store) : false"
								@update:model-value="store[field.key] = $event"
							/>
							<p v-if="field.note && field.note(store)" class="rc-field__note">
								{{ field.note(store) }}
							</p>
						</div>
						<KvRow
							v-else-if="field.kind === 'kv'"
							:label="field.label"
							:value="field.display(store)"
							:tooltip="field.tooltip"
							:color="field.color"
							:clickable="!!field.onClick"
							@click="field.onClick && field.onClick(store)"
						/>
						<label v-else-if="field.kind === 'number'" class="rc-num">
							<span class="rc-num__label">{{ field.label }}</span>
							<input
								type="number"
								class="rc-num__input"
								:placeholder="field.placeholder"
								:value="store[field.key]"
								@input="store[field.key] = toNumber($event)"
							/>
						</label>
						<label v-else-if="field.kind === 'text'" class="rc-num">
							<span class="rc-num__label">{{ field.label }}</span>
							<input
								type="text"
								class="rc-num__input rc-num__input--text"
								:placeholder="field.placeholder"
								:value="store[field.key]"
								@input="store[field.key] = ($event.target as HTMLInputElement).value"
							/>
						</label>
						<KvRow
							v-else-if="field.kind === 'file'"
							:label="field.label"
							:value="field.display(store)"
							:tooltip="field.tooltip"
							clickable
							@click="pickFile(field)"
						/>
					</template>
				</template>
				<p v-if="def.note" class="rc-field__note">{{ def.note }}</p>
			</ConfigCard>

			<ConfigCard v-if="def.outputRows.length" title="Output">
				<KvRow
					v-for="row in def.outputRows"
					:key="row.label"
					:label="row.label"
					:value="row.display(store)"
					:tooltip="row.tooltip"
					:color="row.color"
					:clickable="row.kind !== 'text'"
					@click="openRow(row)"
				/>
			</ConfigCard>

			<ConfigCard v-if="showSafety" title="Safety">
				<div v-if="showConflict" class="rc-conflict-row">
					<span class="rc-conflict-row__label">On conflict</span>
					<ConflictPopover
						:model-value="store.onConflict"
						:rename-disabled="def.renameDisabled"
						@update:model-value="store.onConflict = $event"
					/>
				</div>
				<ToggleSwitch
					v-if="showVerify"
					:model-value="store.verifyAfter"
					:label="def.verifyLabel"
					@update:model-value="store.verifyAfter = $event"
				/>
				<ToggleSwitch
					v-if="showSkip"
					:model-value="store.skipSpaceCheck"
					label="Skip free-space check"
					@update:model-value="store.skipSpaceCheck = $event"
				/>
			</ConfigCard>
		</div>

		<ActionRow :def="def" :store="store" :items="staged" @enqueued="clear" />

		<VerifyResultsCard v-if="def.resultKind === 'verify'" :def="def" />
		<HashResultsCard v-else-if="def.resultKind === 'hash'" />

		<DirectoryPickerModal
			v-if="dirRow"
			:model-value="dirRow.display(store)"
			:default-output-dir="def.defaultOutputDir ?? ''"
			@update:model-value="setDir"
			@close="dirRow = null"
		/>
		<TemplateEditorModal
			v-if="tmplRow"
			:model-value="tmplRow.display(store)"
			@update:model-value="setTmpl"
			@close="tmplRow = null"
		/>
	</div>
</template>

<style scoped>
.rc-page {
	display: flex;
	flex-direction: column;
	gap: 14px;
	padding: 20px 26px;
}

.rc-head {
	display: flex;
	align-items: flex-start;
	justify-content: space-between;
	gap: 16px;
}

.rc-head__title {
	margin: 0;
	font-size: 18px;
	font-weight: 700;
	color: var(--t0);
}

.rc-head__subtitle {
	margin: 4px 0 0;
	font-size: 11.5px;
	color: var(--t4);
}

.rc-grid {
	display: grid;
	grid-template-columns: repeat(auto-fit, minmax(250px, 1fr));
	gap: 12px;
}

.rc-field {
	display: flex;
	flex-direction: column;
	gap: 4px;
}

.rc-field__note {
	margin: 0;
	font-size: 10.5px;
	color: var(--t5);
	line-height: 1.45;
}

.rc-conflict-row {
	display: flex;
	align-items: center;
	justify-content: space-between;
	padding: 3px 0;
}

.rc-conflict-row__label {
	font-size: 12px;
	color: var(--t2);
}

.rc-num {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 10px;
	padding: 3px 0;
}

.rc-num__label {
	font-size: 12px;
	color: var(--t2);
}

.rc-preset-tag {
	font-size: 10px;
	font-weight: 600;
	color: var(--blue);
}

.rc-num__input {
	width: 96px;
	background: var(--bg2);
	border: 1px solid var(--a14);
	border-radius: 6px;
	padding: 4px 8px;
	color: var(--t1);
	font-family: ui-monospace, monospace;
	font-size: 11px;
	text-align: right;
}

.rc-num__input--text {
	width: 190px;
	text-align: left;
}
</style>

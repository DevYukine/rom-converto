<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { open, save } from "~/lib/ipc";
import { useConfigStore } from "~/stores/config";
import { useStaging } from "~/lib/staging";
import { buildCliCommand } from "~/composables/useCliEcho";
import { boundedNumber } from "~/lib/fields";
import { PRESET_BINDINGS } from "~/lib/preset-bindings";
import ConfigCard from "~/components/ui/ConfigCard.vue";
import LevelSlider from "~/components/ui/LevelSlider.vue";
import Segmented from "~/components/ui/Segmented.vue";
import Multiselect from "~/components/ui/Multiselect.vue";
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
import DatScanView from "~/components/op/DatScanView.vue";
import DatRenameView from "~/components/op/DatRenameView.vue";
import DatVerifyView from "~/components/op/DatVerifyView.vue";
import OrganizeView from "~/components/op/OrganizeView.vue";
import { opProgressKey } from "~/lib/opdefs/types";
import type { FieldDef, FileField, OpDef, OutputRow, StagedItem } from "~/lib/opdefs/types";

const props = defineProps<{ def: OpDef }>();

const store = props.def.useStore();
const config = useConfigStore();
const { staged, add, remove, clear } = useStaging(props.def);
const { show: showToast } = useToast();

const presetTag = computed(() =>
	props.def.op === "compress" && props.def.console in PRESET_BINDINGS && config.activePreset ? `from ${config.activePreset}` : "",
);

const cli = computed(() => {
	const name = props.def.browseDirectory || !props.def.acceptedExts.length ? "input" : `input.${props.def.acceptedExts[0]}`;
	const sample: StagedItem = staged.value[0] ?? { id: "", path: name, name, size: 0, outExt: "" };
	const taskId = opProgressKey(props.def, store) ?? "job";
	return buildCliCommand(props.def.buildArgs(store, sample, taskId));
});

const stagedLabel = computed(() => {
	const count = staged.value.length;
	const dirs = staged.value.filter((item) => item.dir).length;
	const noun = dirs === count ? "folder" : dirs === 0 ? "file" : "item";
	const files = `${count} ${count === 1 ? noun : `${noun}s`}`;
	return props.def.resultKind === "verify" ? `Staged: ${files}` : `Staged: ${files}, not queued yet`;
});

const sections = computed(() => {
	const groups = new Map<string, FieldDef[]>();
	groups.set("", []);
	for (const field of props.def.fields) {
		if (!visible(field)) continue;
		const name = field.section ?? "";
		if (!groups.has(name)) groups.set(name, []);
		groups.get(name)!.push(field);
	}
	return [...groups].filter(([, fields]) => fields.length).map(([name, fields]) => ({
		name,
		fields,
		count: fields.filter((field) => {
			if (field.kind === "toggle" && field.disabled?.(store)) return false;
			const value = store[field.key];
			return value === true || typeof value === "number" ||
				(typeof value === "string" && value.length > 0) || (Array.isArray(value) && value.length > 0);
		}).length,
	}));
});
const showOptions = computed(() => sections.value.length > 0);
const cards = computed(() => [
	{ area: "opts", title: props.def.optionsTitle ?? "Options", sections: sections.value.filter((section) => !section.name) },
	{ area: "more", title: "More options", sections: sections.value.filter((section) => section.name) },
].filter((card) => card.sections.length));
const sectionId = useId();
const sectionOpen = ref<Record<string, boolean>>({});
// A collapsed section opens when one of its fields gains a value (a preset or
// a dependent field setting it), never on every edit of an already set value.
const sectionCount: Record<string, number> = {};
watch(sections, (groups) => {
	for (const group of groups) {
		if (!group.name) continue;
		if (!(group.name in sectionOpen.value) || group.count > (sectionCount[group.name] ?? 0)) {
			sectionOpen.value[group.name] = group.count > 0;
		}
		sectionCount[group.name] = group.count;
	}
}, { immediate: true });

const has = (key: string) => key in store;
const showConflict = computed(() => props.def.showConflict !== false && has("onConflict"));
const showVerify = computed(() => !!props.def.showVerify && has("verifyAfter"));
const showSkip = computed(() => has("skipSpaceCheck"));
const showSafety = computed(() => showConflict.value || showVerify.value || showSkip.value);
const showSide = computed(() => props.def.outputRows.length > 0 || showSafety.value);

function visible(field: FieldDef): boolean {
	return field.visible ? field.visible(store) : true;
}

async function pickFile(field: FileField) {
	const picked = await open({ multiple: false, filters: field.filters, directory: field.directory });
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

		<p v-if="def.note && !cards.some((card) => card.area === 'opts')" class="rc-field__note">{{ def.note }}</p>

		<div v-if="def.warning" role="note" class="rc-warning">{{ def.warning }}</div>

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
			@remove="remove"
			@clear="clear"
		/>

		<div class="rc-grid" :class="{ 'rc-grid--no-options': !showOptions, 'rc-grid--solo': !showSide, 'rc-grid--no-more': showOptions && !cards.some((card) => card.area === 'more') }">
			<ConfigCard v-for="card in cards" :key="card.area" :title="card.title" :class="card.area === 'opts' ? 'rc-options' : 'rc-more'">
				<template v-if="card.area === 'opts' && presetTag" #head-tag>
					<span class="rc-preset-tag">{{ presetTag }}</span>
				</template>
				<div v-for="(section, index) in card.sections" :key="section.name" class="rc-section">
					<button
						v-if="section.name"
						type="button"
						class="rc-section__head"
						:class="{ 'rc-section__head--open': sectionOpen[section.name] }"
						:aria-expanded="sectionOpen[section.name]"
						:aria-controls="`${sectionId}-${index}`"
						@click="sectionOpen[section.name] = !sectionOpen[section.name]"
					>
						<span class="rc-section__title">{{ section.name }}</span>
						<span v-if="section.count > 0" class="rc-section__summary">{{ section.count }} set</span>
						<svg class="rc-section__chevron" :class="{ 'rc-section__chevron--open': sectionOpen[section.name] }" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
							<path d="m6 9 6 6 6-6" />
						</svg>
					</button>
					<div v-show="!section.name || sectionOpen[section.name]" :id="section.name ? `${sectionId}-${index}` : undefined" class="rc-section__fields">
						<template v-for="field in section.fields" :key="field.key">
						<LevelSlider
							v-if="field.kind === 'slider'"
							:model-value="store[field.key]"
							:min="field.min"
							:max="field.max"
							:label="field.label"
							:hint="field.hint"
							:tooltip="field.tooltip"
							:format-value="field.formatValue"
							@update:model-value="store[field.key] = $event"
						/>
						<div v-else-if="field.kind === 'segmented'" class="rc-field rc-field--segmented">
							<Segmented
								:model-value="store[field.key]"
								:options="field.options"
								:label="field.label"
								:tooltip="field.tooltip"
								row
								@update:model-value="
									store[field.key] = $event;
									field.onSet?.(store);
								"
							/>
							<p v-if="field.hint" class="rc-field__note">{{ field.hint }}</p>
						</div>
						<div v-else-if="field.kind === 'toggle'" class="rc-field rc-field--toggle">
							<ToggleSwitch
								:model-value="store[field.key]"
								:label="field.label"
								:tooltip="field.tooltip"
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
						<div v-else-if="field.kind === 'number'" class="rc-field rc-field--number">
							<div class="rc-num">
								<FieldLabel :id="`${sectionId}-${field.key}`" :label="field.label" :tooltip="field.tooltip" />
								<input
									type="number"
									class="rc-input rc-input--mono rc-num__input"
									:aria-labelledby="`${sectionId}-${field.key}`"
									:placeholder="field.placeholder"
									:min="field.min"
									:max="field.max"
									:value="store[field.key]"
									@input="store[field.key] = boundedNumber($event, field)"
								/>
							</div>
							<p v-if="field.hint" class="rc-field__note">{{ field.hint }}</p>
						</div>
						<div v-else-if="field.kind === 'select'" class="rc-field rc-field--number">
							<div class="rc-num">
								<FieldLabel :id="`${sectionId}-${field.key}`" :label="field.label" :tooltip="field.tooltip" />
								<select
									class="rc-input rc-input--mono rc-num__select"
									:aria-labelledby="`${sectionId}-${field.key}`"
									:value="store[field.key]"
									@change="store[field.key] = field.options[($event.target as HTMLSelectElement).selectedIndex]!.value"
								>
									<option v-for="option in field.options" :key="option.value" :value="option.value">{{ option.label }}</option>
								</select>
							</div>
							<p v-if="field.hint" class="rc-field__note">{{ field.hint }}</p>
						</div>
						<div v-else-if="field.kind === 'text'" class="rc-field">
							<div class="rc-text">
								<FieldLabel :id="`${sectionId}-${field.key}`" :label="field.label" :tooltip="field.tooltip" />
								<textarea
									v-if="field.multiline"
									class="rc-input rc-input--mono rc-text__input"
									:aria-labelledby="`${sectionId}-${field.key}`"
									:placeholder="field.placeholder"
									:value="store[field.key]"
									@input="store[field.key] = ($event.target as HTMLTextAreaElement).value"
								/>
								<input
									v-else
									type="text"
									class="rc-input rc-input--mono rc-text__input"
									:aria-labelledby="`${sectionId}-${field.key}`"
									:placeholder="field.placeholder"
									:value="store[field.key]"
									@input="store[field.key] = ($event.target as HTMLInputElement).value"
								/>
							</div>
							<p v-if="field.hint" class="rc-field__note">{{ field.hint }}</p>
						</div>
						<div v-else-if="field.kind === 'file'" class="rc-file-row">
							<KvRow
								:label="field.label"
								:value="field.display(store)"
								:tooltip="field.tooltip"
								:color="field.color?.(store)"
								:placeholder="field.placeholder"
								icon="folder"
								clickable
								@click="pickFile(field)"
							/>
							<button
								v-if="store[field.key]"
								type="button"
								class="rc-file-row__clear"
								:aria-label="`Clear ${field.label}`"
								@click="store[field.key] = ''"
							>
								<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
									<path d="m6 6 12 12M6 18 18 6" />
								</svg>
							</button>
						</div>
						<div v-else-if="field.kind === 'multiselect'" class="rc-field">
							<Multiselect
								:model-value="store[field.key]"
								:options="field.options"
								:label="field.label"
								:tooltip="field.tooltip"
								:max="field.max"
								:placeholder="field.placeholder"
								@update:model-value="store[field.key] = $event"
							/>
							<p v-if="field.hint" class="rc-field__note">{{ field.hint }}</p>
						</div>
						</template>
						<p v-if="!section.name && def.note" class="rc-field__note rc-options__note">{{ def.note }}</p>
					</div>
				</div>
			</ConfigCard>

			<div v-if="showSide" class="rc-side">
			<ConfigCard v-if="def.outputRows.length" title="Output">
				<KvRow
					v-for="row in def.outputRows"
					:key="row.label"
					:label="row.label"
					:value="row.display(store)"
					:tooltip="row.tooltip"
					:color="row.color"
					:clickable="row.kind !== 'text'"
					:placeholder="row.placeholder"
					:stacked="row.kind !== 'text'"
					:icon="row.kind === 'template' ? 'edit' : 'folder'"
					@click="openRow(row)"
				/>
			</ConfigCard>

			<ConfigCard v-if="showSafety" title="Safety">
				<div v-if="showConflict" class="rc-conflict-row">
					<FieldLabel
						label="On conflict"
						tooltip="What to do when the output file already exists. The choice is resolved before anything is written."
					/>
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
					:tooltip="def.verifyTooltip ?? 'Runs the same integrity check the verify page does on each output right after it is written.'"
					@update:model-value="store.verifyAfter = $event"
				/>
				<ToggleSwitch
					v-if="showSkip"
					:model-value="store.skipSpaceCheck"
					label="Skip free-space check"
					tooltip="Skips the free space estimate taken before writing. Use it only when the estimate is wrong for your disk."
					@update:model-value="store.skipSpaceCheck = $event"
				/>
			</ConfigCard>
			</div>
			<ActionRow class="rc-grid__actions" :def="def" :store="store" :items="staged" @enqueued="clear" />
		</div>

		<VerifyResultsCard v-if="def.resultKind === 'verify'" :def="def" />
		<HashResultsCard v-else-if="def.resultKind === 'hash'" />
		<DatScanView v-else-if="def.resultKind === 'datScan'" :def="def" />
		<DatVerifyView v-else-if="def.resultKind === 'datVerify'" :def="def" />
		<DatRenameView v-else-if="def.resultKind === 'datRename'" :def="def" />
		<OrganizeView v-else-if="def.resultKind === 'organize'" :def="def" />

		<DirectoryPickerModal
			v-if="dirRow"
			:model-value="dirRow.value ? dirRow.value(store) : dirRow.display(store)"
			:clear-label="dirRow.required ? '' : dirRow.placeholder"
			@update:model-value="setDir"
			@close="dirRow = null"
		/>
		<TemplateEditorModal
			v-if="tmplRow"
			:model-value="tmplRow.display(store)"
			:placeholder="tmplRow.placeholder"
			@update:model-value="setTmpl"
			@close="tmplRow = null"
		/>
	</div>
</template>

<style scoped>
.rc-page {
	display: flex;
	flex-direction: column;
	gap: 16px;
	padding: 24px 28px 32px;
}

.rc-head {
	display: flex;
	flex-wrap: wrap;
	align-items: flex-start;
	gap: 10px 24px;
}

.rc-head__text {
	flex: 1 1 480px;
	min-width: 0;
}

.rc-head :deep(.rc-cli-chip) {
	flex: 0 1 auto;
	min-width: 0;
	max-width: min(480px, 100%);
}

.rc-head__title {
	margin: 0;
	font-size: var(--fs-xl);
	line-height: 1.25;
	font-weight: 700;
	color: var(--t0);
	text-wrap: balance;
}

.rc-head__subtitle {
	margin: 4px 0 0;
	max-width: 72ch;
	font-size: var(--fs-md);
	line-height: var(--lh-body);
	color: var(--t4);
	text-wrap: pretty;
}

.rc-grid {
	display: grid;
	grid-template-columns: minmax(0, 1fr);
	grid-template-areas: "opts" "side" "more" "actions";
	gap: 16px;
	align-items: start;
}

.rc-options {
	grid-area: opts;
}

.rc-more {
	grid-area: more;
}

.rc-grid__actions {
	grid-area: actions;
}

.rc-side {
	grid-area: side;
	display: flex;
	flex-direction: column;
	gap: 16px;
	min-width: 0;
}

.rc-grid--no-options {
	grid-template-areas: "side" "actions";
}

.rc-grid--no-more {
	grid-template-areas: "opts" "side" "actions";
}

.rc-grid--solo {
	grid-template-areas: "opts" "more" "actions";
}

.rc-grid--solo.rc-grid--no-more {
	grid-template-areas: "opts" "actions";
}

.rc-grid--no-options.rc-grid--solo {
	grid-template-areas: "actions";
	grid-template-rows: auto;
}

.rc-grid :deep(.rc-config-card__body > * + *),
.rc-section__fields > * + *,
.rc-section + .rc-section {
	border-top: 1px solid var(--a06);
}

.rc-section__head {
	display: flex;
	align-items: center;
	gap: 12px;
	width: 100%;
	padding: 10px 0;
	border: none;
	border-radius: var(--r-sm);
	background: none;
	color: var(--t1);
	text-align: left;
	cursor: pointer;
}

.rc-section__head:hover {
	background: var(--a04);
}

.rc-section__head--open {
	padding-top: 16px;
}

.rc-section__title {
	flex: 1 1 auto;
	font-size: var(--fs-lg);
	font-weight: 600;
}

.rc-section__summary {
	flex: none;
	font-size: var(--fs-sm);
	color: var(--blue);
	white-space: nowrap;
}

.rc-section__chevron {
	flex: none;
	color: var(--t4);
}

.rc-section__chevron--open {
	transform: rotate(180deg);
}

.rc-field {
	display: flex;
	flex-direction: column;
	padding: 6px 0;
}

.rc-field--segmented {
	container: field / inline-size;
	min-height: 40px;
	justify-content: center;
}

.rc-file-row {
	display: flex;
	align-items: center;
	gap: 6px;
	min-width: 0;
}

.rc-file-row :deep(.rc-kv) {
	flex: 1 1 auto;
	min-width: 0;
}

.rc-file-row__clear {
	display: inline-flex;
	align-items: center;
	justify-content: center;
	flex: none;
	width: var(--ctl-h);
	height: var(--ctl-h);
	border: 1px solid var(--a14);
	border-radius: var(--r-sm);
	background: none;
	color: var(--t5);
	cursor: pointer;
}

.rc-file-row__clear:hover {
	border-color: var(--a30);
	color: var(--t3);
}

.rc-field--toggle,
.rc-field--number {
	padding: 0;
}

.rc-field--toggle > .rc-field__note,
.rc-field--number > .rc-field__note {
	padding-bottom: 6px;
}

.rc-field__note {
	margin: 4px 0 0;
	font-size: var(--fs-sm);
	color: var(--t5);
	line-height: var(--lh-body);
	text-wrap: pretty;
}

.rc-options__note {
	padding: 6px 0;
}

.rc-conflict-row,
.rc-num {
	display: flex;
	flex-wrap: wrap;
	align-items: center;
	gap: 16px;
	min-height: 40px;
	padding: 6px 0;
}


.rc-conflict-row > :deep(.rc-field-label),
.rc-num > :deep(.rc-field-label) {
	flex: 1 1 auto;
	min-width: 0;
}

.rc-text {
	display: flex;
	flex-direction: column;
	gap: 6px;
}

.rc-text__input {
	width: 100%;
}

.rc-num__input {
	flex: none;
	width: 112px;
	text-align: right;
}

.rc-num__select {
	flex: none;
	width: 176px;
	max-width: 100%;
}

.rc-preset-tag {
	font-size: var(--fs-xs);
	font-weight: 600;
	color: var(--blue);
	white-space: nowrap;
}

@container page (min-width: 820px) {
	.rc-grid {
		grid-template-columns: minmax(0, 1fr) minmax(280px, 340px);
		grid-template-areas: "opts side" "more side" "actions side";
		grid-template-rows: max-content max-content minmax(0, 1fr);
		justify-content: start;
	}

	.rc-grid--no-more {
		grid-template-areas: "opts side" "actions side";
		grid-template-rows: max-content minmax(0, 1fr);
	}

	.rc-side {
		position: sticky;
		top: 16px;
	}

	.rc-grid--no-options {
		grid-template-columns: minmax(0, 1fr);
		grid-template-areas: "side" "actions";
		grid-template-rows: max-content minmax(0, 1fr);
	}

	.rc-grid--no-options .rc-side {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
		align-items: start;
		position: static;
	}

	.rc-grid--solo {
		grid-template-columns: minmax(0, 1fr);
		grid-template-areas: "opts" "more" "actions";
	}

	.rc-grid--solo.rc-grid--no-more {
		grid-template-areas: "opts" "actions";
		grid-template-rows: max-content minmax(0, 1fr);
	}
}
</style>

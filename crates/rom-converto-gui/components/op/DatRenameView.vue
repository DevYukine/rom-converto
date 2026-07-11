<script setup lang="ts">
import { computed, ref } from "vue";
import { storeToRefs } from "pinia";
import { invoke } from "~/lib/ipc";
import { basename } from "~/composables/useDerivedPath";
import { useProgress } from "~/composables/useProgress";
import { useDatRenameStore } from "~/stores/datRename";
import ConfigCard from "~/components/ui/ConfigCard.vue";
import ConflictPopover from "~/components/modals/ConflictPopover.vue";
import StatusTag from "~/components/ui/StatusTag.vue";
import DropZone from "~/components/op/DropZone.vue";

type DatRenameAction =
	| "renamed"
	| "would-rename"
	| "already-canonical"
	| "skip-unmatched"
	| "skip-weak"
	| "skip-collision"
	| "skip-disc-set"
	| "failed";

interface DatRenameRow {
	from: string;
	to: string | null;
	action: DatRenameAction;
	detail: string | null;
}

interface DatRenameResult {
	kind: "rename";
	dryRun: boolean;
	renamed: number;
	skipped: number;
	failed: number;
	rows: DatRenameRow[];
}

const store = useDatRenameStore();
const { input, maxDepth, onConflict, loading, error } = storeToRefs(store);
const progress = useProgress("dat-rename");

const renameResult = ref<DatRenameResult | null>(null);

const TAG: Record<DatRenameAction, { tag: string; label: string }> = {
	renamed: { tag: "RENAMED", label: "Renamed" },
	"would-rename": { tag: "MISNAMED", label: "Would rename" },
	"already-canonical": { tag: "MATCHED", label: "Canonical" },
	"skip-unmatched": { tag: "UNKNOWN", label: "Skip: unmatched" },
	"skip-weak": { tag: "UNKNOWN", label: "Skip: weak match" },
	"skip-collision": { tag: "UNKNOWN", label: "Skip: collision" },
	"skip-disc-set": { tag: "UNKNOWN", label: "Skip: disc set" },
	failed: { tag: "FAILED", label: "Failed" },
};

const pendingCount = computed(() => (renameResult.value?.rows ?? []).filter((r) => r.action === "would-rename").length);
const applied = computed(() => !!renameResult.value && !renameResult.value.dryRun && pendingCount.value === 0);

function onDepthInput(e: Event) {
	const raw = (e.target as HTMLInputElement).value;
	maxDepth.value = raw === "" ? null : Number(raw);
}

async function setDir(paths: string[]) {
	if (!paths[0]) return;
	input.value = paths[0];
	await run(true);
}

async function run(dry: boolean) {
	if (!input.value || loading.value) return;
	progress.reset();
	renameResult.value = null;
	loading.value = true;
	error.value = "";
	const args = { input: input.value, maxDepth: maxDepth.value, dryRun: dry, onConflict: onConflict.value };
	try {
		const json = await invoke<string>("cmd_dat_rename", args);
		renameResult.value = JSON.parse(json) as DatRenameResult;
	} catch (e: unknown) {
		const msg = typeof e === "string" ? e : (e as Error)?.message ?? String(e);
		if (!msg.includes("operation cancelled")) error.value = msg;
	} finally {
		loading.value = false;
	}
}

// Apply re-runs the full pipeline with dryRun false rather than replaying the
// preview plan, so filesystem changes between preview and apply are re-planned.
function apply() {
	void run(false);
}
</script>

<template>
	<div class="rc-page">
		<div class="rc-head">
			<div class="rc-head__text">
				<h1 class="rc-head__title">Rename to canonical</h1>
				<p class="rc-head__subtitle">
					Renames files to their canonical Playmatch DAT names. Only hash-verified matches are renamed.
				</p>
			</div>
			<div class="rc-head__actions">
				<button
					type="button"
					class="rc-apply"
					:class="{ 'rc-apply--done': applied }"
					:disabled="loading || pendingCount === 0"
					@click="apply"
				>
					{{ applied ? "All renamed ✓" : `Rename all (${pendingCount})` }}
				</button>
			</div>
		</div>

		<DropZone
			:drop-text="input || 'Drop a folder or file to preview renames'"
			:multiple="false"
			also-directory
			@add="setDir"
		/>

		<ConfigCard title="Options">
			<label class="rc-num">
				<span class="rc-num__label">Max depth</span>
				<input
					type="number"
					min="1"
					class="rc-num__input"
					placeholder="Unlimited"
					:value="maxDepth ?? ''"
					@input="onDepthInput"
				>
			</label>
			<div class="rc-conflict-row">
				<span class="rc-conflict-row__label">On conflict</span>
				<ConflictPopover :model-value="onConflict" @update:model-value="onConflict = $event" />
			</div>
		</ConfigCard>

		<div v-if="renameResult" class="rc-results">
			<div class="rc-results__head">
				<span>{{ pendingCount }} file{{ pendingCount === 1 ? "" : "s" }} to rename</span>
				<span class="rc-results__note">Only the filename changes. The file content is never touched.</span>
			</div>
			<div v-for="r in renameResult.rows" :key="r.from" class="rc-row">
				<StatusTag :status="TAG[r.action].tag" :label="TAG[r.action].label" :width="96" />
				<div class="rc-row__text">
					<span class="rc-row__name">{{ basename(r.from) }}</span>
					<span v-if="r.to" class="rc-row__to">↳ {{ basename(r.to) }}</span>
					<span v-else-if="r.detail" class="rc-row__detail">{{ r.detail }}</span>
				</div>
			</div>
		</div>

		<div v-if="error" class="rc-error">{{ error }}</div>

		<div v-if="loading" class="rc-progress">
			<div class="rc-progress__track">
				<div class="rc-progress__fill" :style="{ width: `${progress.percent.value}%` }" />
			</div>
		</div>
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
	max-width: 520px;
	line-height: 1.45;
}

.rc-head__actions {
	display: flex;
	align-items: center;
	gap: 8px;
	flex-shrink: 0;
}

.rc-apply {
	border: none;
	border-radius: 8px;
	padding: 7px 16px;
	font-size: 12px;
	font-weight: 700;
	color: #fff;
	background: #2f6fd0;
	cursor: pointer;
}

.rc-apply:disabled {
	background: var(--btnDim);
	cursor: not-allowed;
}

.rc-apply--done:disabled {
	background: var(--btnDim);
	color: var(--t3);
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

.rc-num__input {
	width: 110px;
	background: var(--bg2);
	border: 1px solid var(--a14);
	border-radius: 6px;
	padding: 4px 8px;
	color: var(--t1);
	font-family: ui-monospace, monospace;
	font-size: 11px;
	text-align: right;
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

.rc-results {
	border: 1px solid var(--a10);
	border-radius: 10px;
	background: var(--card);
	overflow: hidden;
}

.rc-results__head {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 10px;
	padding: 10px 14px;
	border-bottom: 1px solid var(--a06);
	font-size: 11.5px;
	color: var(--t2);
}

.rc-results__note {
	font-size: 11px;
	color: var(--t5);
}

.rc-row {
	display: flex;
	align-items: center;
	gap: 12px;
	padding: 8px 14px;
	border-top: 1px solid var(--a06);
}

.rc-row__text {
	display: flex;
	flex-direction: column;
	gap: 2px;
	min-width: 0;
	flex: 1;
}

.rc-row__name {
	color: var(--t0);
	font-size: 12px;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-row__to {
	color: var(--green);
	font-size: 11px;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-row__detail {
	color: var(--t4);
	font-size: 11px;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-error {
	border-left: 2px solid var(--red);
	background: rgba(212, 58, 62, 0.06);
	border-radius: 8px;
	padding: 10px 14px;
	font-size: 12px;
	color: var(--red);
}

.rc-progress {
	display: flex;
	align-items: center;
	gap: 10px;
}

.rc-progress__track {
	flex: 1;
	height: 4px;
	border-radius: 3px;
	background: var(--a10);
	overflow: hidden;
}

.rc-progress__fill {
	height: 100%;
	background: #2f6fd0;
	transition: width 0.15s linear;
}
</style>

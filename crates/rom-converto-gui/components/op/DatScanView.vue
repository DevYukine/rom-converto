<script setup lang="ts">
import { computed } from "vue";
import { storeToRefs } from "pinia";
import { invoke } from "~/lib/ipc";
import { basename } from "~/composables/useDerivedPath";
import { useProgress } from "~/composables/useProgress";
import { useDatScanStore } from "~/stores/datScan";
import type { DatScanRowEvent, DatScanResult, DatScanStatus, ScanLevel } from "~/stores/datScan";
import { useAlertsStore } from "~/stores/alerts";
import ConfigCard from "~/components/ui/ConfigCard.vue";
import Segmented from "~/components/ui/Segmented.vue";
import ToggleSwitch from "~/components/ui/ToggleSwitch.vue";
import FilterChip from "~/components/ui/FilterChip.vue";
import StatusTag from "~/components/ui/StatusTag.vue";
import DetailModal from "~/components/modals/DetailModal.vue";
import DropZone from "~/components/op/DropZone.vue";

const store = useDatScanStore();
const { input, maxDepth, scanLevel, quick, scanResult, liveRows, statusFilter, loading, error } = storeToRefs(store);
const alerts = useAlertsStore();
const progress = useProgress("dat-scan");

void store.ensureRowListener();

const SCAN_LEVELS: { label: string; value: ScanLevel }[] = [
	{ label: "CRC + Size", value: "crc" },
	{ label: "MD5", value: "md5" },
	{ label: "SHA-1", value: "sha1" },
	{ label: "SHA-256", value: "sha256" },
];

// Every level keeps crc32: it is near-free alongside the stronger digest and
// stays the fallback match rung.
const SCAN_LEVEL_ALGOS: Record<ScanLevel, string[]> = {
	crc: ["crc32"],
	md5: ["crc32", "md5"],
	sha1: ["crc32", "sha1"],
	sha256: ["crc32", "sha256"],
};

type Chip = { status: DatScanStatus; label: string; color: "green" | "yellow" | "neutral" | "red" };
const CHIPS: Chip[] = [
	{ status: "matched", label: "Matched", color: "green" },
	{ status: "misnamed", label: "Misnamed", color: "yellow" },
	{ status: "hint", label: "Hint", color: "yellow" },
	{ status: "unknown", label: "Unknown", color: "neutral" },
	{ status: "unsupported", label: "Unsupported", color: "neutral" },
	{ status: "failed", label: "Failed", color: "red" },
];

const TAG: Record<string, { tag: string; label: string }> = {
	matched: { tag: "MATCHED", label: "Matched" },
	misnamed: { tag: "MISNAMED", label: "Misnamed" },
	hint: { tag: "HINT", label: "Hint" },
	unknown: { tag: "UNKNOWN", label: "Unknown" },
	unsupported: { tag: "UNSUPPORTED", label: "Unsupported" },
	failed: { tag: "FAILED", label: "Failed" },
	pending: { tag: "pending", label: "Pending" },
};

const sourceRows = computed<DatScanRowEvent[]>(() =>
	scanResult.value ? scanResult.value.rows : Array.from(liveRows.value.values()),
);

const counts = computed<Record<string, number>>(() => {
	const c: Record<string, number> = {};
	for (const r of sourceRows.value) c[r.status] = (c[r.status] ?? 0) + 1;
	return c;
});

const visibleRows = computed(() =>
	sourceRows.value.filter((r) => statusFilter.value === "all" || r.status === statusFilter.value),
);

const filterLabel = computed(() =>
	statusFilter.value === "all" ? "all files" : (TAG[statusFilter.value]?.label ?? statusFilter.value),
);

const showRenameLink = computed(() => statusFilter.value === "all" || statusFilter.value === "misnamed");

function toggleFilter(status: DatScanStatus) {
	statusFilter.value = statusFilter.value === status ? "all" : status;
}

function detail(r: DatScanRowEvent): { text: string; tone: "green" | "red" | "muted" } | null {
	if (r.status === "failed") return r.error ? { text: r.error, tone: "red" } : null;
	if (r.status === "misnamed") {
		const to = r.canonicalStem ?? r.gameName;
		return to ? { text: `↳ ${to}`, tone: "green" } : null;
	}
	if (r.gameName) return { text: r.gameName, tone: "green" };
	return null;
}

const detailRow = ref<DatScanRowEvent | null>(null);

function onDepthInput(e: Event) {
	const raw = (e.target as HTMLInputElement).value;
	maxDepth.value = raw === "" ? null : Number(raw);
}

function setDir(paths: string[]) {
	if (paths[0]) input.value = paths[0];
}

const hasScanned = computed(() => !!scanResult.value || liveRows.value.size > 0);

const router = useRouter();

async function rescan() {
	if (!input.value || loading.value) return;
	progress.reset();
	store.clearScanState();
	loading.value = true;
	error.value = "";
	const args = { input: input.value, maxDepth: maxDepth.value, algos: SCAN_LEVEL_ALGOS[scanLevel.value], quick: quick.value };
	try {
		const json = await invoke<string>("cmd_dat_scan", args);
		const parsed = JSON.parse(json) as DatScanResult;
		scanResult.value = parsed;
		alerts.push({
			type: "plain",
			title: "DAT scan finished",
			body: `${parsed.matched} matched · ${parsed.misnamed} misnamed · ${parsed.unknown} unknown · ${parsed.failed} failed`,
			meta: `${input.value} · just now`,
		});
	} catch (e: unknown) {
		const msg = typeof e === "string" ? e : (e as Error)?.message ?? String(e);
		if (!msg.includes("operation cancelled")) error.value = msg;
	} finally {
		loading.value = false;
	}
}

function cancel() {
	void invoke("cmd_cancel", { taskId: "dat-scan" });
}
</script>

<template>
	<div class="rc-page">
		<div class="rc-head">
			<div class="rc-head__text">
				<h1 class="rc-head__title">Scan library</h1>
				<p class="rc-head__subtitle">
					Matches each file against the Playmatch DAT database, streaming results live. Cancel keeps partial results.
				</p>
			</div>
			<div class="rc-head__actions">
				<button v-if="!loading" type="button" class="rc-toggle rc-toggle--go" :disabled="!input" @click="rescan">
					{{ hasScanned ? "Rescan" : "Scan" }}
				</button>
				<button v-else type="button" class="rc-toggle rc-toggle--stop" @click="cancel">Cancel</button>
			</div>
		</div>

		<DropZone
			:drop-text="input || 'Drop a folder to scan'"
			:multiple="false"
			directory
			@add="setDir"
		/>

		<ConfigCard title="Scan level">
			<Segmented
				:model-value="scanLevel"
				:options="SCAN_LEVELS"
				@update:model-value="scanLevel = $event as ScanLevel"
			/>
			<p class="rc-caption">Quick scan trusts zip CRC32 where possible and falls back automatically.</p>
			<ToggleSwitch :model-value="quick" label="Quick scan" @update:model-value="quick = $event" />
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
		</ConfigCard>

		<div v-if="sourceRows.length" class="rc-chips">
			<FilterChip
				label="All"
				:count="sourceRows.length"
				:active="statusFilter === 'all'"
				@click="statusFilter = 'all'"
			/>
			<FilterChip
				v-for="chip in CHIPS"
				:key="chip.status"
				:label="chip.label"
				:count="counts[chip.status] ?? 0"
				:color="chip.color"
				:active="statusFilter === chip.status"
				@click="toggleFilter(chip.status)"
			/>
		</div>

		<div v-if="sourceRows.length" class="rc-results">
			<div class="rc-results__head">
				<span>Showing <strong>{{ filterLabel }}</strong>. Click a chip to filter</span>
				<button v-if="showRenameLink" type="button" class="rc-link" @click="router.push('/dat/rename')">
					· Rename all to canonical…
				</button>
			</div>
			<div v-for="r in visibleRows" :key="r.path" class="rc-row">
				<StatusTag :status="TAG[r.status]?.tag ?? r.status" :label="TAG[r.status]?.label" />
				<div class="rc-row__text">
					<span class="rc-row__name">{{ basename(r.path) }}</span>
					<span
						v-if="detail(r)"
						class="rc-row__detail"
						:class="`rc-row__detail--${detail(r)!.tone}`"
					>{{ detail(r)!.text }}</span>
				</div>
				<button v-if="r.status === 'failed'" type="button" class="rc-link" @click="detailRow = r">Details</button>
			</div>
		</div>

		<div v-if="error" class="rc-error">{{ error }}</div>

		<div v-if="loading || progress.total.value > 0" class="rc-progress">
			<div class="rc-progress__track">
				<div class="rc-progress__fill" :style="{ width: `${progress.percent.value}%` }" />
			</div>
			<span class="rc-progress__label">{{ progress.current.value }} / {{ progress.total.value }} files</span>
		</div>

		<DetailModal
			v-if="detailRow"
			:title="basename(detailRow.path)"
			:lines="[detailRow.error ?? 'No additional detail.']"
			@close="detailRow = null"
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
	max-width: 520px;
	line-height: 1.45;
}

.rc-head__actions {
	display: flex;
	align-items: center;
	gap: 8px;
	flex-shrink: 0;
}

.rc-toggle {
	border: none;
	border-radius: 8px;
	padding: 7px 16px;
	font-size: 12px;
	font-weight: 700;
	color: #fff;
	cursor: pointer;
}

.rc-toggle:disabled {
	background: var(--btnDim);
	cursor: not-allowed;
}

.rc-toggle--go {
	background: #2f6fd0;
}

.rc-toggle--stop {
	background: #d43a3e;
}

.rc-caption {
	margin: 2px 0 0;
	font-size: 10.5px;
	color: var(--t5);
	line-height: 1.4;
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

.rc-chips {
	display: flex;
	flex-wrap: wrap;
	gap: 8px;
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
	gap: 6px;
	padding: 10px 14px;
	border-bottom: 1px solid var(--a06);
	font-size: 11.5px;
	color: var(--t4);
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

.rc-row__detail {
	font-size: 11px;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-row__detail--green {
	color: var(--green);
}

.rc-row__detail--red {
	color: var(--red);
}

.rc-row__detail--muted {
	color: var(--t4);
}

.rc-link {
	border: none;
	background: none;
	color: var(--blue);
	font-size: 11.5px;
	cursor: pointer;
	padding: 0;
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

.rc-progress__label {
	font-family: ui-monospace, monospace;
	font-size: 10.5px;
	color: var(--t5);
	white-space: nowrap;
}
</style>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { storeToRefs } from "pinia";
import { invoke } from "~/lib/ipc";
import { basename } from "~/composables/useDerivedPath";
import { buildCliCommand } from "~/composables/useCliEcho";
import { openContextMenu } from "~/composables/useContextMenu";
import { useProgress } from "~/composables/useProgress";
import { rowContextItems, useResultRows } from "~/composables/useResultRows";
import { useToast } from "~/composables/useToast";
import { createRateMeter, formatElapsed, formatEta, formatRate, relativePath } from "~/lib/scan-stats";
import { useDatScanStore } from "~/stores/datScan";
import type { DatScanRowEvent, DatScanResult, DatScanStatus, ScanLevel } from "~/stores/datScan";
import { useAlertsStore } from "~/stores/alerts";
import CliChip from "~/components/ui/CliChip.vue";
import ConfigCard from "~/components/ui/ConfigCard.vue";
import Segmented from "~/components/ui/Segmented.vue";
import ToggleSwitch from "~/components/ui/ToggleSwitch.vue";
import FilterChip from "~/components/ui/FilterChip.vue";
import StatusTag from "~/components/ui/StatusTag.vue";
import VirtualList from "~/components/ui/VirtualList.vue";
import DetailModal from "~/components/modals/DetailModal.vue";
import DropZone from "~/components/op/DropZone.vue";

const store = useDatScanStore();
const { input, maxDepth, scanLevel, quick, scanResult, liveRows, statusFilter, loading, error, startedAt, finishedAt } =
	storeToRefs(store);
const alerts = useAlertsStore();
const { show: showToast } = useToast();
const progress = useProgress("dat-scan");
const fileProgress = useProgress("dat-scan-file");

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

type Chip = { status: DatScanStatus | "pending"; label: string; color: "green" | "yellow" | "neutral" | "red" };
const CHIPS: Chip[] = [
	{ status: "matched", label: "Matched", color: "green" },
	{ status: "misnamed", label: "Misnamed", color: "yellow" },
	{ status: "hint", label: "Hint", color: "yellow" },
	{ status: "unknown", label: "Unknown", color: "neutral" },
	{ status: "unsupported", label: "Unsupported", color: "neutral" },
	{ status: "failed", label: "Failed", color: "red" },
	{ status: "pending", label: "Pending", color: "neutral" },
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

const ROW_HEIGHT = 46;

const scanArgs = computed(() => ({
	input: input.value,
	maxDepth: maxDepth.value,
	algos: SCAN_LEVEL_ALGOS[scanLevel.value],
	quick: quick.value,
}));
const cli = computed(() => buildCliCommand("cmd_dat_scan", scanArgs.value));

const sourceRows = computed<DatScanRowEvent[]>(() => scanResult.value?.rows ?? liveRows.value);

const { counts, visibleRows: statusRows, toggleFilter } = useResultRows(sourceRows, (r) => r.status, statusFilter);

const query = ref("");
const visibleRows = computed(() => {
	const q = query.value.trim().toLowerCase();
	if (!q) return statusRows.value;
	return statusRows.value.filter(
		(r) => relativePath(r.path, scanRoot.value).toLowerCase().includes(q) || r.gameName?.toLowerCase().includes(q),
	);
});

const chips = computed(() => CHIPS.filter((c) => c.status !== "pending" || (counts.value.pending ?? 0) > 0));
watch(
	() => counts.value.pending ?? 0,
	(n) => {
		if (n === 0 && statusFilter.value === "pending") statusFilter.value = "all";
	},
);

const filterLabel = computed(() =>
	statusFilter.value === "all" ? "all files" : (TAG[statusFilter.value]?.label ?? statusFilter.value),
);

const showRenameLink = computed(
	() => (counts.value.misnamed ?? 0) > 0 && (statusFilter.value === "all" || statusFilter.value === "misnamed"),
);

const summary = computed(() => {
	if (!scanResult.value) return "";
	const n = scanResult.value.rows.length;
	const took = finishedAt.value > startedAt.value ? ` in ${formatElapsed(finishedAt.value - startedAt.value)}` : "";
	return `${n.toLocaleString()} file${n === 1 ? "" : "s"} scanned${took}`;
});

const noFiles = computed(() => !!scanResult.value && scanResult.value.rows.length === 0);

const indeterminate = computed(() => progress.total.value === 0);

// Throughput in files per second, smoothed so a burst of tiny files or one
// large image does not swing the estimate; the ETA is derived from it.
const meter = createRateMeter();
const rate = ref(0);
watch(progress.current, (n) => {
	if (progress.total.value > 0) rate.value = meter.sample(performance.now(), n);
});

const countLabel = computed(() =>
	indeterminate.value
		? ""
		: `${progress.current.value.toLocaleString()} of ${progress.total.value.toLocaleString()} files`,
);
const rateLabel = computed(() => (indeterminate.value ? "" : formatRate(rate.value)));
const etaLabel = computed(() => {
	if (indeterminate.value || rate.value <= 0) return "";
	return formatEta((progress.total.value - progress.current.value) / rate.value);
});
// The current file name is throttled: folders of tiny files would otherwise
// blur through dozens of names per second. Its own percentage only matters
// for images large enough to sit on the bar for a while.
const FILE_NAME_MS = 250;
const FILE_PCT_MIN = 32 * 1024 * 1024;
const currentFile = ref("");
let fileNameAt = 0;
let fileNameTimer: ReturnType<typeof setTimeout> | null = null;
function showFileName() {
	fileNameTimer = null;
	fileNameAt = performance.now();
	currentFile.value = fileProgress.message.value;
}
watch(fileProgress.message, () => {
	const wait = FILE_NAME_MS - (performance.now() - fileNameAt);
	if (wait <= 0) showFileName();
	else fileNameTimer ??= setTimeout(showFileName, wait);
});
watch(indeterminate, (busy) => {
	if (busy) currentFile.value = "";
});
const showFileBar = computed(() => fileProgress.running.value && fileProgress.total.value >= FILE_PCT_MIN);

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

function contextItems(r: DatScanRowEvent) {
	return rowContextItems(r.path, detail(r)?.text);
}

function onDepthInput(e: Event) {
	const raw = (e.target as HTMLInputElement).value;
	maxDepth.value = raw === "" ? null : Number(raw);
}

function setDir(paths: string[]) {
	if (paths[0]) input.value = paths[0];
}

const hasScanned = computed(() => !!scanResult.value || liveRows.value.length > 0);

// Rows stay labelled against the folder that was scanned even if the field
// is edited afterwards.
const scanRoot = ref("");

const router = useRouter();

async function rescan() {
	if (!input.value || loading.value) return;
	progress.reset();
	fileProgress.reset();
	store.clearScanState();
	query.value = "";
	scanRoot.value = input.value;
	meter.reset();
	rate.value = 0;
	if (fileNameTimer) clearTimeout(fileNameTimer);
	fileNameTimer = null;
	fileNameAt = 0;
	currentFile.value = "";
	loading.value = true;
	error.value = "";
	startedAt.value = Date.now();
	finishedAt.value = 0;
	try {
		const json = await invoke<string>("cmd_dat_scan", scanArgs.value);
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
		store.flushLiveRows();
		finishedAt.value = Date.now();
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
				<CliChip :command="cli" @copy="showToast('Copied')" />
				<button v-if="!loading" type="button" class="rc-toggle rc-toggle--go" :disabled="!input" @click="rescan">
					{{ hasScanned ? "Rescan" : "Scan" }}
				</button>
				<button v-else type="button" class="rc-toggle rc-toggle--stop" @click="cancel">Cancel</button>
			</div>
		</div>

		<div v-if="loading" class="rc-progress" role="status" aria-live="polite">
			<div class="rc-progress__row">
				<span class="rc-progress__phase">{{ progress.message.value || "Starting" }}</span>
				<span v-if="countLabel" class="rc-progress__count">{{ countLabel }}</span>
				<span class="rc-progress__stats">
					<template v-if="rateLabel">{{ rateLabel }}</template>
					<template v-if="rateLabel && etaLabel"> · </template>
					<template v-if="etaLabel">{{ etaLabel }}</template>
				</span>
			</div>
			<div class="rc-progress__track" :class="{ 'rc-progress__track--busy': indeterminate }">
				<div class="rc-progress__fill" :style="{ width: indeterminate ? '' : `${progress.percent.value}%` }" />
			</div>
			<div v-if="currentFile" class="rc-progress__file">
				<span class="rc-progress__file-name">{{ currentFile }}</span>
				<span v-if="showFileBar" class="rc-progress__file-pct">{{ fileProgress.percent.value }}%</span>
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
				label="Level"
				tooltip="CRC32 plus size identifies almost everything. Raise this to MD5, SHA-1, or SHA-256 only when a match needs a stronger digest."
				@update:model-value="scanLevel = $event as ScanLevel"
			/>
			<p class="rc-caption">Quick scan trusts zip CRC32 where possible and falls back automatically.</p>
			<ToggleSwitch
				:model-value="quick"
				label="Quick scan"
				tooltip="Trusts a zip's own CRC32 for eligible cartridge images instead of extracting and hashing. Falls back automatically when that alone does not verify."
				@update:model-value="quick = $event"
			/>
			<label class="rc-num">
				<FieldLabel label="Max depth" tooltip="Folder levels to scan. Leave it empty for unlimited." />
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

		<div v-for="w in progress.warnings.value" :key="w" role="note" class="rc-warning">{{ w }}</div>

		<div v-if="error" class="rc-error">{{ error }}</div>

		<div v-if="noFiles" class="rc-empty">No files found under {{ input }}.</div>

		<div v-if="sourceRows.length" class="rc-chips">
			<FilterChip
				label="All"
				:count="sourceRows.length"
				:active="statusFilter === 'all'"
				@click="statusFilter = 'all'"
			/>
			<FilterChip
				v-for="chip in chips"
				:key="chip.status"
				:label="chip.label"
				:count="counts[chip.status] ?? 0"
				:color="chip.color"
				:active="statusFilter === chip.status"
				:class="{ 'rc-chip--empty': !(counts[chip.status] ?? 0) }"
				@click="toggleFilter(chip.status)"
			/>
		</div>

		<div v-if="sourceRows.length" class="rc-results">
			<div class="rc-results__head">
				<span class="rc-results__showing">
					<template v-if="summary">{{ summary }} · </template>Showing <strong>{{ filterLabel }}</strong>
					<template v-if="query"> matching “{{ query }}”</template>
					<template v-if="visibleRows.length !== sourceRows.length"> ({{ visibleRows.length.toLocaleString() }})</template>
				</span>
				<input
					v-model="query"
					type="search"
					class="rc-results__search"
					placeholder="Filter by name"
					aria-label="Filter results by name"
				>
				<button v-if="showRenameLink" type="button" class="rc-link" @click="router.push('/dat/rename')">
					Rename all to canonical…
				</button>
			</div>
			<div v-if="!visibleRows.length" class="rc-results__none">Nothing matches this filter.</div>
			<VirtualList v-else :items="visibleRows" :row-height="ROW_HEIGHT" :key-of="(r) => r.path">
				<template #default="{ item: r }">
					<div class="rc-row" @contextmenu="openContextMenu($event, contextItems(r))">
						<StatusTag :status="TAG[r.status]?.tag ?? r.status" :label="TAG[r.status]?.label" />
						<div class="rc-row__text">
							<span class="rc-row__name" :title="r.path">{{ relativePath(r.path, scanRoot) }}</span>
							<span
								v-if="detail(r)"
								class="rc-row__detail"
								:class="`rc-row__detail--${detail(r)!.tone}`"
							>{{ detail(r)!.text }}</span>
						</div>
						<button v-if="r.status === 'failed'" type="button" class="rc-link" @click="detailRow = r">Details</button>
					</div>
				</template>
			</VirtualList>
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
	max-width: 46%;
}

.rc-head__actions :deep(.rc-cli-chip) {
	min-width: 0;
}

.rc-toggle {
	border: none;
	border-radius: 8px;
	padding: 7px 16px;
	font-size: 12px;
	font-weight: 700;
	color: #fff;
	cursor: pointer;
	flex-shrink: 0;
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

.rc-progress {
	position: sticky;
	top: 0;
	z-index: 2;
	display: flex;
	flex-direction: column;
	gap: 7px;
	padding: 10px 14px;
	border: 1px solid var(--a10);
	border-radius: 10px;
	background: var(--card);
	box-shadow: 0 6px 18px -10px var(--shC);
}

.rc-progress__row {
	display: flex;
	align-items: baseline;
	gap: 10px;
	font-size: 11.5px;
}

.rc-progress__phase {
	font-weight: 600;
	color: var(--t1);
}

.rc-progress__count {
	font-family: ui-monospace, monospace;
	color: var(--t3);
}

.rc-progress__stats {
	margin-left: auto;
	color: var(--t5);
	white-space: nowrap;
}

.rc-progress__track {
	position: relative;
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

.rc-progress__track--busy .rc-progress__fill {
	position: absolute;
	width: 30%;
	animation: rc-busy 1.2s ease-in-out infinite;
}

@keyframes rc-busy {
	from {
		left: -30%;
	}

	to {
		left: 100%;
	}
}

.rc-progress__file {
	display: flex;
	gap: 8px;
	font-family: ui-monospace, monospace;
	font-size: 10.5px;
	color: var(--t5);
}

.rc-progress__file-name {
	min-width: 0;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-progress__file-pct {
	margin-left: auto;
}

.rc-warning {
	border: 1px solid rgba(210, 153, 34, 0.4);
	border-radius: 10px;
	background: rgba(210, 153, 34, 0.1);
	padding: 10px 14px;
	font-size: 12px;
	line-height: 1.5;
	color: var(--yellow);
}

.rc-empty {
	border: 1px dashed var(--a14);
	border-radius: 10px;
	padding: 18px 14px;
	text-align: center;
	font-size: 12px;
	color: var(--t4);
}

.rc-chips {
	display: flex;
	flex-wrap: wrap;
	gap: 8px;
}

.rc-chip--empty {
	opacity: 0.55;
}

.rc-results {
	border: 1px solid var(--a10);
	border-radius: 10px;
	background: var(--card);
	overflow: hidden;
	user-select: text;
}

.rc-results__head {
	display: flex;
	align-items: center;
	gap: 12px;
	padding: 8px 14px;
	border-bottom: 1px solid var(--a06);
	font-size: 11.5px;
	color: var(--t4);
}

.rc-results__showing {
	flex: 1;
	min-width: 0;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-results__search {
	width: 180px;
	background: var(--bg2);
	border: 1px solid var(--a14);
	border-radius: 6px;
	padding: 4px 8px;
	color: var(--t1);
	font-size: 11px;
}

.rc-results__search:focus {
	outline: none;
	border-color: var(--a30);
}

.rc-results__none {
	padding: 14px;
	font-size: 11.5px;
	color: var(--t5);
	text-align: center;
}

.rc-row {
	display: flex;
	align-items: center;
	gap: 12px;
	height: 100%;
	padding: 0 14px;
	border-top: 1px solid var(--a06);
	box-sizing: border-box;
}

.rc-row:hover {
	background: var(--a03);
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
</style>

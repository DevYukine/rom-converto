<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { storeToRefs } from "pinia";
import { basename } from "~/composables/useDerivedPath";
import { openContextMenu } from "~/composables/useContextMenu";
import { useProgress } from "~/composables/useProgress";
import { rowContextItems, useResultRows } from "~/composables/useResultRows";
import { createRateMeter, formatElapsed, formatEta, formatRate, relativePath } from "~/lib/scan-stats";
import { useDatScanStore } from "~/stores/datScan";
import { useQueueStore, type QueueJob } from "~/stores/queue";
import { opProgressKey, requestPath } from "~/lib/opdefs/types";
import type { DatScanData, DatScanRow } from "~/types";
import type { OpDef } from "~/lib/opdefs/types";
import ConfigCard from "~/components/ui/ConfigCard.vue";
import FilterChip from "~/components/ui/FilterChip.vue";
import StatusTag from "~/components/ui/StatusTag.vue";
import VirtualList from "~/components/ui/VirtualList.vue";
import DetailModal from "~/components/modals/DetailModal.vue";

const props = defineProps<{ def: OpDef }>();

const store = useDatScanStore();
const queue = useQueueStore();
const router = useRouter();
const { statusFilter, liveRows } = storeToRefs(store);

const progressKey = opProgressKey(props.def, store) ?? props.def.storeId;
const progress = useProgress(progressKey);
const fileProgress = useProgress(`${progressKey}-file`);

void store.ensureRowListener();

const ROW_HEIGHT = 46;

const CHIPS: { status: string; label: string; color: "green" | "yellow" | "neutral" | "red" }[] = [
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

function mine(job: QueueJob): boolean {
	return job.resultKind === "datScan" && job.routeBack?.storeId === props.def.storeId;
}

const activeJob = computed(() =>
	queue.jobs.find((j) => mine(j) && (j.status === "queued" || j.status === "running")),
);
const runningJob = computed(() => queue.jobs.find((j) => mine(j) && j.status === "running"));
const lastJob = computed<QueueJob | null>(() => {
	for (let i = queue.finished.length - 1; i >= 0; i--) {
		const job = queue.finished[i]!;
		if (mine(job)) return job;
	}
	return null;
});
const shownJob = computed<QueueJob | null>(() => activeJob.value ?? lastJob.value);

// Rows stay labelled against the folder the shown run scanned, whatever the
// input field holds now.
const scanRoot = computed(() => (shownJob.value ? requestPath(shownJob.value.args, "input") : ""));

const resultRows = computed<DatScanRow[] | null>(() => {
	const result = lastJob.value?.result;
	if (!result || typeof result === "string") return null;
	return (result.data as DatScanData | null)?.rows ?? null;
});

// Streamed rows stand in while the run is live and after a cancel, which
// leaves the job without a result payload at all.
const sourceRows = computed<DatScanRow[]>(() =>
	activeJob.value ? liveRows.value : (resultRows.value ?? liveRows.value),
);

const { counts, visibleRows: statusRows, toggleFilter } = useResultRows(sourceRows, (r) => r.status, statusFilter);

const query = ref("");
const visibleRows = computed(() => {
	const q = query.value.trim().toLowerCase();
	if (!q) return statusRows.value;
	return statusRows.value.filter(
		(r) =>
			relativePath(r.path, scanRoot.value).toLowerCase().includes(q) ||
			r.game_name?.toLowerCase().includes(q),
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
	const rows = resultRows.value;
	const job = lastJob.value;
	if (activeJob.value || !rows || !job) return "";
	const elapsed = (job.finishedAt ?? 0) - (job.startedAt ?? 0);
	const took = job.startedAt && elapsed > 0 ? ` in ${formatElapsed(elapsed)}` : "";
	return `${rows.length.toLocaleString()} file${rows.length === 1 ? "" : "s"} scanned${took}`;
});

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

// Each run starts from an empty stream; staging alone must not drop the
// results already on screen. The tail of the buffer is folded in when the run
// ends, so a cancelled run keeps every row it did produce.
watch(
	() => runningJob.value?.id,
	(id, prev) => {
		if (id) {
			store.clearScanState();
			fileProgress.reset();
			query.value = "";
			meter.reset();
			rate.value = 0;
			if (fileNameTimer) clearTimeout(fileNameTimer);
			fileNameTimer = null;
			fileNameAt = 0;
			currentFile.value = "";
		} else if (prev) {
			store.flushLiveRows();
		}
	},
);

function detail(r: DatScanRow): { text: string; tone: "green" | "red" | "muted" } | null {
	if (r.status === "failed") return r.error ? { text: r.error, tone: "red" } : null;
	if (r.status === "misnamed") {
		const to = r.canonical_stem ?? r.game_name;
		return to ? { text: `↳ ${to}`, tone: "green" } : null;
	}
	if (r.game_name) return { text: r.game_name, tone: "green" };
	return null;
}

const detailRow = ref<DatScanRow | null>(null);

function contextItems(r: DatScanRow) {
	return rowContextItems(r.path, detail(r)?.text);
}
</script>

<template>
	<div v-if="runningJob" class="rc-progress" role="status" aria-live="polite">
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

	<div v-for="w in progress.warnings.value" :key="w" role="note" class="rc-warning">{{ w }}</div>

	<ConfigCard v-if="sourceRows.length" title="Results">
		<template #head-tag>
			<button v-if="showRenameLink" type="button" class="rc-link" @click="router.push('/dat/rename')">
				Rename all to canonical…
			</button>
		</template>

		<div class="rc-chips">
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
	</ConfigCard>

	<DetailModal
		v-if="detailRow"
		:title="basename(detailRow.path)"
		:lines="[detailRow.error ?? 'No additional detail.']"
		@close="detailRow = null"
	/>
</template>

<style scoped>
.rc-progress {
	display: flex;
	flex-direction: column;
	gap: 7px;
	padding: 10px 14px;
	border: 1px solid var(--a10);
	border-radius: 10px;
	background: var(--card);
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

.rc-chips {
	display: flex;
	flex-wrap: wrap;
	gap: 8px;
	padding-bottom: 4px;
}

.rc-chip--empty {
	opacity: 0.55;
}

.rc-results__head {
	display: flex;
	align-items: center;
	gap: 12px;
	padding: 6px 0;
	border-top: 1px solid var(--a06);
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
	padding: 0;
	border-top: 1px solid var(--a06);
	box-sizing: border-box;
	user-select: text;
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
</style>

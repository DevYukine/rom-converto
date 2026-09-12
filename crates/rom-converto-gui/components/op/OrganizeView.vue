<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { storeToRefs } from "pinia";
import { basename } from "~/composables/useDerivedPath";
import { openContextMenu } from "~/composables/useContextMenu";
import { useProgress } from "~/composables/useProgress";
import { useResultRows } from "~/composables/useResultRows";
import { relativePath } from "~/lib/scan-stats";
import { useOrganizeStore } from "~/stores/organize";
import { useQueueStore, type QueueJob } from "~/stores/queue";
import { opProgressKey, requestPath } from "~/lib/opdefs/types";
import type { OrganizeData, OrganizeRow } from "~/types";
import type { OpDef } from "~/lib/opdefs/types";
import ConfigCard from "~/components/ui/ConfigCard.vue";
import FilterChip from "~/components/ui/FilterChip.vue";
import StatusTag from "~/components/ui/StatusTag.vue";
import VirtualList from "~/components/ui/VirtualList.vue";
import DetailModal from "~/components/modals/DetailModal.vue";

const props = defineProps<{ def: OpDef }>();

const store = useOrganizeStore();
const queue = useQueueStore();
const { statusFilter, liveRows } = storeToRefs(store);

const progressKey = opProgressKey(props.def, store) ?? props.def.storeId;
const progress = useProgress(progressKey);
const fileProgress = useProgress(`${progressKey}-file`);

void store.ensureRowListener();

const ROW_HEIGHT = 46;

const CHIPS = [
	{ status: "ok", label: "Ok", color: "green" },
	{ status: "skipped", label: "Skipped", color: "yellow" },
	{ status: "failed", label: "Failed", color: "red" },
] as const;

const TAG: Record<string, { tag: string; label: string }> = {
	ok: { tag: "PASSED", label: "Ok" },
	skipped: { tag: "UNKNOWN", label: "Skipped" },
	failed: { tag: "FAILED", label: "Failed" },
};

function mine(job: QueueJob): boolean {
	return job.resultKind === "organize" && job.routeBack?.storeId === props.def.storeId;
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

// Rows stay labelled against the folders the shown run used, whatever the
// form fields hold now.
const libraryRoot = computed(() => (shownJob.value ? requestPath(shownJob.value.args, "input") : ""));
const outputRoot = computed(() => shownJob.value?.args.request.options.output_dir ?? "");

const plan = computed<OrganizeData | null>(() => {
	const result = lastJob.value?.result;
	if (!result || typeof result === "string") return null;
	return (result.data as OrganizeData | null) ?? null;
});

// Streamed rows stand in while the run is live and after a cancel, which
// leaves the job without a result payload at all.
const sourceRows = computed<OrganizeRow[]>(() =>
	activeJob.value ? liveRows.value : (plan.value?.rows ?? liveRows.value),
);

const { counts, visibleRows: statusRows, toggleFilter } = useResultRows(sourceRows, (r) => r.status, statusFilter);

const query = ref("");
const visibleRows = computed(() => {
	const q = query.value.trim().toLowerCase();
	if (!q) return statusRows.value;
	return statusRows.value.filter(
		(r) =>
			relativePath(r.input, libraryRoot.value).toLowerCase().includes(q) ||
			(r.output ? relativePath(r.output, outputRoot.value).toLowerCase().includes(q) : false) ||
			(r.console ?? "").toLowerCase().includes(q),
	);
});

const summary = computed(() => {
	const text = `${counts.value.ok ?? 0} ok · ${counts.value.skipped ?? 0} skipped · ${counts.value.failed ?? 0} failed`;
	const playlists = plan.value?.playlists.length ?? 0;
	return playlists ? `${text} · ${playlists} playlists` : text;
});

const isPlan = computed(() => !!plan.value?.dry_run && plan.value.rows.some((r) => r.planned));

// A queued or running organize job must not be queued a second time.
const applying = computed(() =>
	queue.jobs.some((j) => mine(j) && (j.status === "queued" || j.status === "running")),
);

// Apply re-queues the same request without the dry-run flag rather than
// replaying the preview plan, so filesystem changes since the preview are
// re-planned.
function apply() {
	const job = lastJob.value;
	if (!job || applying.value) return;
	const taskId = `job-${crypto.randomUUID()}`;
	queue.enqueue([
		{
			name: job.name,
			opLabel: job.opLabel,
			command: job.command,
			args: { ...job.args, taskId, request: { ...job.args.request, dry_run: false } },
			taskId,
			progressKey: job.progressKey,
			chips: job.chips,
			resultKind: "organize",
			routeBack: job.routeBack,
		},
	]);
}

// Each run starts from an empty stream; staging alone must not drop the
// results already on screen. The tail of the buffer is folded in when the run
// ends, so a cancelled run keeps every row it did produce.
watch(
	() => runningJob.value?.id,
	(id, prev) => {
		if (id) {
			store.clearRunState();
			fileProgress.reset();
			query.value = "";
		} else if (prev) {
			store.flushLiveRows();
		}
	},
);

function detail(r: OrganizeRow): { text: string; tone: "green" | "red" | "muted" } | null {
	if (!r.detail) return null;
	if (r.status === "failed") return { text: r.detail, tone: "red" };
	if (r.planned) return { text: r.detail, tone: "green" };
	if (r.status === "skipped") return { text: r.detail, tone: "muted" };
	return null;
}

const detailRow = ref<OrganizeRow | null>(null);

function contextItems(r: OrganizeRow) {
	const items = [{ label: "Copy file path", value: r.input }];
	if (r.output) items.push({ label: "Copy output path", value: r.output });
	const rowText = [basename(r.input), r.output ? basename(r.output) : detail(r)?.text].filter(Boolean).join(" · ");
	items.push({ label: "Copy row", value: rowText });
	return items;
}
</script>

<template>
	<div v-if="runningJob" class="rc-progress" role="status" aria-live="polite">
		<div class="rc-progress__row">
			<span class="rc-progress__phase">{{ progress.message.value || "Starting" }}</span>
			<span v-if="progress.total.value" class="rc-progress__count">
				{{ progress.current.value.toLocaleString() }} of {{ progress.total.value.toLocaleString() }}
			</span>
		</div>
		<div class="rc-progress__track" :class="{ 'rc-progress__track--busy': progress.total.value === 0 }">
			<div class="rc-progress__fill" :style="{ width: progress.total.value === 0 ? '' : `${progress.percent.value}%` }" />
		</div>
		<div v-if="fileProgress.message.value" class="rc-progress__file">
			<span class="rc-progress__file-name">{{ fileProgress.message.value }}</span>
			<span
				v-if="fileProgress.running.value && fileProgress.total.value"
				class="rc-progress__file-pct"
			>{{ fileProgress.percent.value }}%</span>
		</div>
	</div>

	<div v-for="w in progress.warnings.value" :key="w" role="note" class="rc-warning">{{ w }}</div>

	<ConfigCard v-if="sourceRows.length" title="Results">
		<template #head-tag>
			<button v-if="isPlan && !applying" type="button" class="rc-link" @click="apply">
				Apply — run for real…
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
				v-for="chip in CHIPS"
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
				<template v-if="summary">{{ summary }} · </template>Showing
				<strong>{{ statusFilter === "all" ? "all files" : (TAG[statusFilter]?.label ?? statusFilter) }}</strong>
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
		<VirtualList v-else :items="visibleRows" :row-height="ROW_HEIGHT" :key-of="(r) => r.input">
			<template #default="{ item: r }">
				<div class="rc-row" @contextmenu="openContextMenu($event, contextItems(r))">
					<StatusTag v-if="r.planned" status="BASE" label="Planned" :width="58" />
					<StatusTag :status="TAG[r.status]?.tag ?? r.status" :label="TAG[r.status]?.label" />
					<div class="rc-row__text">
						<span class="rc-row__path">
							<span class="rc-row__console">{{ r.console || "-" }}</span>
							<span class="rc-row__action">{{ r.action }}</span>
							<span class="rc-row__name" :title="r.input">{{ relativePath(r.input, libraryRoot) }}</span>
							<span class="rc-row__arrow">→</span>
							<span
								v-if="r.output"
								class="rc-row__name"
								:title="r.output"
							>{{ relativePath(r.output, outputRoot) }}</span>
							<span v-else class="rc-row__arrow">-</span>
						</span>
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
		:title="basename(detailRow.input)"
		:lines="[detailRow.detail ?? 'No additional detail.']"
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

.rc-row__path {
	display: flex;
	align-items: baseline;
	gap: 6px;
	min-width: 0;
}

.rc-row__console {
	flex-shrink: 0;
	font-size: 11px;
	color: var(--t3);
}

.rc-row__action {
	flex-shrink: 0;
	font-family: ui-monospace, monospace;
	font-size: 10.5px;
	color: var(--blue);
}

.rc-row__name {
	color: var(--t0);
	font-size: 12px;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-row__arrow {
	flex-shrink: 0;
	color: var(--t5);
	font-size: 11px;
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

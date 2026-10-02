<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
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

const resultsElement = ref<HTMLElement | null>(null);
const stacked = ref(false);
let resultsObserver: ResizeObserver | null = null;
watch(resultsElement, (el) => {
	resultsObserver?.disconnect();
	if (!el) return;
	const container = el.closest<HTMLElement>(".rc-results")!;
	resultsObserver = new ResizeObserver(([entry]) => {
		stacked.value = entry!.contentRect.width < 960;
	});
	resultsObserver.observe(container);
}, { flush: "post" });
onBeforeUnmount(() => resultsObserver?.disconnect());

const CHIPS = [
	{ status: "ok", label: "OK", color: "green" },
	{ status: "skipped", label: "Skipped", color: "neutral" },
	{ status: "failed", label: "Failed", color: "red" },
] as const;

const TAG: Record<string, { tag: string; label: string }> = {
	ok: { tag: "PASSED", label: "OK" },
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
	const playlists = plan.value?.playlists.length ?? 0;
	return playlists ? `${playlists} playlist${playlists === 1 ? "" : "s"}` : "";
});

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

function detail(r: OrganizeRow): { text: string; tone: "red" | "muted" } | null {
	if (!r.detail) return null;
	if (r.status === "failed") return { text: r.detail, tone: "red" };
	if (r.status === "skipped") return { text: r.detail, tone: "muted" };
	return null;
}

function rowHeight(row: OrganizeRow): number {
	return detail(row) ? 60 : 44;
}

const ACTION_LABELS: Record<string, string> = {
	compress: "Compress", zip: "Zip", copy: "Copy", move: "Move",
	link: "Link", clean: "Clean", playlist: "Playlist", skip: "",
};
const SUFFIX_LABELS: Record<string, string> = {
	migrate: "Migrate", convert: "Convert", decrypt: "Decrypt", compress: "Compress",
};

function actionLabel(action: string): string {
	return ACTION_LABELS[action] ?? SUFFIX_LABELS[action.slice(action.lastIndexOf(".") + 1)] ?? action;
}

function consoleLabel(row: OrganizeRow): string {
	return row.action === "clean" || row.action === "playlist" ? "" : row.console || "Unknown";
}

function directory(path: string, root: string): string {
	const relative = relativePath(path, root);
	return relative.slice(0, relative.length - basename(path).length);
}

// The tail keeps the extension and disc number visible when the middle is truncated.
const NAME_TAIL = 14;

function splitName(path: string): [string, string] {
	const name = basename(path);
	return [name.slice(0, -NAME_TAIL), name.slice(-NAME_TAIL)];
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

	<ConfigCard v-if="sourceRows.length" title="Results" class="rc-results">
		<div ref="resultsElement" class="rc-organize">
		<div class="rc-results__toolbar">
			<div class="rc-results__chips">
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
					:class="{ 'rc-results__chip--empty': !(counts[chip.status] ?? 0) }"
					@click="toggleFilter(chip.status)"
				/>
			</div>
			<input
				v-model="query"
				type="search"
				class="rc-input rc-results__search"
				placeholder="Filter by name"
				aria-label="Filter results by name"
			>
		</div>

		<p class="rc-results__summary">
			<template v-if="summary">{{ summary }} · </template>Showing
			<strong>{{ statusFilter === "all" ? "all files" : (TAG[statusFilter]?.label ?? statusFilter) }}</strong>
			<template v-if="query"> matching “{{ query }}”</template>
			<template v-if="visibleRows.length !== sourceRows.length"> ({{ visibleRows.length.toLocaleString() }})</template>
		</p>

		<div v-if="!visibleRows.length" class="rc-results__none">Nothing matches this filter.</div>
		<template v-else>
			<div class="rc-results__columns" aria-hidden="true">
				<span>Status</span>
				<span class="rc-columns__stack">File</span>
				<span class="rc-columns__wide">Console</span>
				<span class="rc-columns__wide">Action</span>
				<span class="rc-columns__wide">Source</span>
				<span class="rc-columns__wide" />
				<span class="rc-columns__wide">Destination</span>
				<span class="rc-results__actions" />
			</div>
			<VirtualList :items="visibleRows" :row-height="stacked ? 64 : rowHeight" :key-of="(r, index) => `${index}:${r.input}`">
				<template #default="{ item: r }">
					<div class="rc-results__row" :class="{ 'rc-results__row--fail': r.status === 'failed', 'rc-results__row--multiline': !!detail(r) }" @contextmenu="openContextMenu($event, contextItems(r))">
						<StatusTag :status="TAG[r.status]?.tag ?? r.status" :label="TAG[r.status]?.label" />
						<div class="rc-results__text">
							<div class="rc-row__meta">
								<span class="rc-row__console" :class="{ 'rc-row__console--unknown': !r.console }" :title="consoleLabel(r)">{{ consoleLabel(r) }}</span>
								<span class="rc-row__action" :title="r.action">{{ actionLabel(r.action) }}</span>
							</div>
							<div class="rc-row__path" :class="{ 'rc-row__path--source-only': !r.output }">
								<span class="rc-results__name rc-row__source" :title="r.input">
									<span class="rc-row__directory">{{ directory(r.input, libraryRoot) }}</span><span class="rc-row__basename"><span class="rc-row__basename-start">{{ splitName(r.input)[0] }}</span><span class="rc-row__basename-end">{{ splitName(r.input)[1] }}</span></span>
								</span>
								<span class="rc-results__arrow">{{ r.output ? "→" : "" }}</span>
								<span class="rc-results__name rc-row__destination" :title="r.output ?? undefined">
									<template v-if="r.output"><span class="rc-row__directory">{{ directory(r.output, outputRoot) }}</span><span class="rc-row__basename"><span class="rc-row__basename-start">{{ splitName(r.output)[0] }}</span><span class="rc-row__basename-end">{{ splitName(r.output)[1] }}</span></span></template>
								</span>
							</div>
							<span
								v-if="detail(r)"
								class="rc-results__detail"
								:class="`rc-results__detail--${detail(r)!.tone}`"
								:title="detail(r)!.text"
							>{{ detail(r)!.text }}</span>
						</div>
						<div class="rc-results__actions">
							<button v-if="r.status === 'failed'" type="button" class="rc-results__link" @click="detailRow = r">Details</button>
						</div>
					</div>
				</template>
			</VirtualList>
		</template>
		</div>
	</ConfigCard>

	<DetailModal
		v-if="detailRow"
		:title="basename(detailRow.input)"
		:lines="[detailRow.detail ?? 'No additional detail.']"
		tone="error"
		@close="detailRow = null"
	/>
</template>

<style scoped>
.rc-organize .rc-results__columns,
.rc-organize .rc-results__row {
	grid-template-columns: 96px minmax(128px, 0.7fr) 88px minmax(0, 1.5fr) 16px minmax(0, 1.5fr) auto;
}

.rc-columns__stack {
	display: none;
}

.rc-organize .rc-results__row {
	height: 100%;
	padding-top: 0;
	padding-bottom: 0;
}

.rc-organize .rc-results__text {
	display: grid;
	grid-template-columns: subgrid;
	grid-column: 2 / 7;
	align-items: center;
	row-gap: 2px;
}

.rc-row__meta,
.rc-row__path {
	display: contents;
}

.rc-row__console,
.rc-row__action {
	min-width: 0;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
	font-size: var(--fs-sm);
	color: var(--t3);
}

.rc-row__console--unknown {
	color: var(--t6);
}

.rc-organize .rc-results__name {
	display: flex;
	font-size: var(--fs-sm);
	font-family: var(--font-mono);
}

.rc-row__directory {
	flex: 0 1 auto;
	min-width: 0;
	overflow: hidden;
	text-overflow: ellipsis;
	color: var(--t5);
}

.rc-row__basename {
	display: flex;
	flex: none;
	max-width: 100%;
	min-width: 0;
}

.rc-row__basename-start {
	min-width: 0;
	overflow: hidden;
	text-overflow: ellipsis;
}

.rc-row__basename-end {
	flex: none;
}

.rc-organize .rc-results__detail {
	grid-column: 3 / -1;
}

.rc-organize .rc-results__row--multiline .rc-row__console,
.rc-organize .rc-results__row--multiline .rc-row__action {
	grid-row: 1 / span 2;
}

@container results (width < 960px) {
	.rc-organize .rc-results__columns,
	.rc-organize .rc-results__row {
		grid-template-columns: 96px minmax(0, 1fr) auto;
	}

	.rc-columns__wide {
		display: none;
	}

	.rc-columns__stack {
		display: block;
	}

	.rc-organize .rc-results__text {
		grid-column: 2;
		grid-template-columns: minmax(0, 1fr);
	}

	.rc-row__meta {
		display: flex;
		align-items: baseline;
		gap: 6px;
		min-width: 0;
	}

	.rc-row__console:not(:empty) + .rc-row__action:not(:empty)::before {
		content: "·";
		margin-right: 6px;
		color: var(--t5);
	}

	.rc-row__path {
		display: grid;
		grid-template-columns: minmax(0, 1fr) 16px minmax(0, 1fr);
		gap: 4px;
		min-width: 0;
	}

	.rc-row__path--source-only .rc-row__source {
		grid-column: 1 / -1;
	}

	.rc-row__path--source-only .rc-results__arrow,
	.rc-row__path--source-only .rc-row__destination {
		display: none;
	}

	.rc-organize .rc-results__detail {
		grid-column: 1 / -1;
	}
}
</style>

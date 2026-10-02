<script setup lang="ts">
import { computed, ref } from "vue";
import { basename } from "~/composables/useDerivedPath";
import { openContextMenu } from "~/composables/useContextMenu";
import { useQueueStore, type QueueJob } from "~/stores/queue";
import type { DatRenameData, DatRenameRowData } from "~/types";
import type { OpDef } from "~/lib/opdefs/types";
import ConfigCard from "~/components/ui/ConfigCard.vue";
import StatusTag from "~/components/ui/StatusTag.vue";

const props = defineProps<{ def: OpDef }>();

const queue = useQueueStore();

const TAG: Record<string, { tag: string; label: string }> = {
	renamed: { tag: "RENAMED", label: "Renamed" },
	would_rename: { tag: "MISNAMED", label: "Would rename" },
	already_canonical: { tag: "MATCHED", label: "Canonical" },
	skipped: { tag: "UNKNOWN", label: "Skipped" },
	skip_unmatched: { tag: "UNKNOWN", label: "Skip: unmatched" },
	skip_weak: { tag: "UNKNOWN", label: "Skip: weak match" },
	skip_collision: { tag: "UNKNOWN", label: "Skip: collision" },
	skip_disc_set: { tag: "UNKNOWN", label: "Skip: disc set" },
	failed: { tag: "FAILED", label: "Failed" },
};

const lastJob = computed<QueueJob | null>(() => {
	for (let i = queue.finished.length - 1; i >= 0; i--) {
		const job = queue.finished[i]!;
		if (job.resultKind !== "datRename" || job.routeBack?.storeId !== props.def.storeId) continue;
		if (job.status !== "done" || !job.result || typeof job.result === "string") continue;
		return job;
	}
	return null;
});

const plan = computed<DatRenameData | null>(() => {
	const result = lastJob.value?.result;
	if (!result || typeof result === "string") return null;
	return (result.data as DatRenameData | null) ?? null;
});

const pendingCount = computed(() => plan.value?.rows.filter((r) => r.action === "would_rename").length ?? 0);
const applied = computed(() => !!plan.value && !plan.value.dry_run);
const query = ref("");
const visibleRows = computed(() => {
	const q = query.value.trim().toLowerCase();
	return (plan.value?.rows ?? []).filter((r) => !q || r.from.toLowerCase().includes(q) || r.to?.toLowerCase().includes(q));
});
// A queued or running rename must not be queued a second time: the plan on
// screen is already being re-planned against the filesystem.
const running = computed(() =>
	queue.jobs.some(
		(j) =>
			j.resultKind === "datRename" &&
			j.routeBack?.storeId === props.def.storeId &&
			(j.status === "queued" || j.status === "running"),
	),
);

// Apply re-queues the same request without the dry-run flag rather than
// replaying the preview plan, so filesystem changes since the preview are
// re-planned.
function apply() {
	const job = lastJob.value;
	if (!job || running.value) return;
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
			resultKind: "datRename",
			routeBack: job.routeBack,
		},
	]);
}

function contextItems(r: DatRenameRowData) {
	const items = [{ label: "Copy file path", value: r.from }];
	if (r.to) items.push({ label: "Copy new name", value: basename(r.to) });
	if (r.detail) items.push({ label: "Copy detail", value: r.detail });
	const rowText = [basename(r.from), r.to ? basename(r.to) : r.detail].filter(Boolean).join(" · ");
	items.push({ label: "Copy row", value: rowText });
	return items;
}
</script>

<template>
	<ConfigCard v-if="plan" title="Rename plan" class="rc-results rc-rename">
		<div class="rc-results__toolbar">
			<button type="button" class="rc-apply" :disabled="pendingCount === 0 || running" @click="apply">
				{{ running ? "Renaming…" : applied ? "All renamed ✓" : `Rename all (${pendingCount})` }}
			</button>
			<input v-model="query" type="search" class="rc-input rc-results__search" placeholder="Filter by name" aria-label="Filter results by name">
		</div>
		<p class="rc-results__summary">{{ visibleRows.length }} of {{ plan.rows.length }} {{ plan.rows.length === 1 ? "file" : "files" }}. Only the filename changes. The file content is never touched.</p>
		<div class="rc-results__columns" aria-hidden="true">
			<span>Status</span>
			<span class="rc-columns__stack">Name</span>
			<span class="rc-columns__wide">Original</span>
			<span class="rc-columns__wide" />
			<span class="rc-columns__wide">New name</span>
		</div>
		<div v-if="!visibleRows.length" class="rc-results__none">Nothing matches this filter.</div>
		<div
			v-for="r in visibleRows"
			:key="r.from"
			class="rc-results__row"
			:class="{ 'rc-results__row--fail': r.action === 'failed', 'rc-results__row--multiline': !r.to && !!r.detail, 'rc-rename-row--new-name': !!r.to }"
			@contextmenu="openContextMenu($event, contextItems(r))"
		>
			<StatusTag :status="TAG[r.action]?.tag ?? r.action" :label="TAG[r.action]?.label" />
			<div class="rc-results__text">
				<span class="rc-results__name" :title="r.from">{{ basename(r.from) }}</span>
				<span class="rc-results__arrow">{{ r.to ? "→" : "" }}</span>
				<span class="rc-row__to" :title="r.to ?? undefined">{{ r.to ? basename(r.to) : "" }}</span>
				<span v-if="!r.to && r.detail" class="rc-results__detail" :title="r.detail">{{ r.detail }}</span>
			</div>
		</div>
	</ConfigCard>
</template>

<style scoped>
.rc-apply {
	flex: none;
	height: 32px;
	border: none;
	border-radius: var(--r-md);
	padding: 0 16px;
	font-size: var(--fs-md);
	font-weight: 600;
	color: #fff;
	background: var(--fill);
	white-space: nowrap;
	cursor: pointer;
}

.rc-apply:hover:not(:disabled) {
	background: var(--fill-hover);
}

.rc-apply:disabled {
	background: var(--a08);
	color: var(--t5);
	cursor: not-allowed;
}

.rc-rename .rc-results__columns,
.rc-rename .rc-results__row {
	grid-template-columns: 128px minmax(0, 1fr) 16px minmax(0, 1fr);
}

.rc-columns__stack {
	display: none;
}

.rc-rename .rc-results__text {
	display: grid;
	grid-template-columns: subgrid;
	grid-column: 2 / -1;
	align-items: center;
}

.rc-row__to {
	min-width: 0;
	font-size: var(--fs-sm);
	color: var(--green);
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-rename .rc-results__detail {
	grid-column: 1 / -1;
}

@container results (max-width: 519px) {
	.rc-rename .rc-results__columns,
	.rc-rename .rc-results__row {
		grid-template-columns: 128px minmax(0, 1fr);
	}

	.rc-columns__wide,
	.rc-rename .rc-results__arrow {
		display: none;
	}

	.rc-columns__stack {
		display: block;
	}

	.rc-rename .rc-results__text {
		grid-template-columns: minmax(0, 1fr);
	}

	.rc-rename-row--new-name {
		min-height: 60px;
	}
}
</style>

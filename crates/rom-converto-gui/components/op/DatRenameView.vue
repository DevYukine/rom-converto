<script setup lang="ts">
import { computed } from "vue";
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
	<ConfigCard v-if="plan" title="Rename plan">
		<template #head-tag>
			<span class="rc-head">
				<span class="rc-head__note">Only the filename changes. The file content is never touched.</span>
				<button type="button" class="rc-apply" :disabled="pendingCount === 0 || running" @click="apply">
					{{ running ? "Renaming…" : applied ? "All renamed ✓" : `Rename all (${pendingCount})` }}
				</button>
			</span>
		</template>

		<div
			v-for="r in plan.rows"
			:key="r.from"
			class="rc-row"
			@contextmenu="openContextMenu($event, contextItems(r))"
		>
			<StatusTag :status="TAG[r.action]?.tag ?? r.action" :label="TAG[r.action]?.label" :width="96" />
			<div class="rc-row__text">
				<span class="rc-row__name">{{ basename(r.from) }}</span>
				<span v-if="r.to" class="rc-row__to">↳ {{ basename(r.to) }}</span>
				<span v-else-if="r.detail" class="rc-row__detail">{{ r.detail }}</span>
			</div>
		</div>
	</ConfigCard>
</template>

<style scoped>
.rc-head {
	display: flex;
	align-items: center;
	gap: 10px;
}

.rc-head__note {
	font-size: 11px;
	font-weight: 400;
	text-transform: none;
	letter-spacing: 0;
	color: var(--t5);
}

.rc-apply {
	border: none;
	border-radius: 8px;
	padding: 5px 14px;
	font-size: 11.5px;
	font-weight: 700;
	color: #fff;
	background: #2f6fd0;
	cursor: pointer;
}

.rc-apply:disabled {
	background: var(--btnDim);
	color: var(--t3);
	cursor: not-allowed;
}

.rc-row {
	display: flex;
	align-items: center;
	gap: 12px;
	padding: 8px 0;
	border-top: 1px solid var(--a06);
	user-select: text;
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
</style>

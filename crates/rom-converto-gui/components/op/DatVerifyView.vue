<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { storeToRefs } from "pinia";
import { basename } from "~/composables/useDerivedPath";
import { openContextMenu } from "~/composables/useContextMenu";
import { rowContextItems, useResultRows } from "~/composables/useResultRows";
import { useDatVerifyStore } from "~/stores/datVerify";
import { useQueueStore, type QueueJob } from "~/stores/queue";
import type { DatMatchData, DatVerifyData } from "~/types";
import type { OpDef } from "~/lib/opdefs/types";
import ConfigCard from "~/components/ui/ConfigCard.vue";
import FilterChip from "~/components/ui/FilterChip.vue";
import StatusTag from "~/components/ui/StatusTag.vue";
import DetailModal from "~/components/modals/DetailModal.vue";

const props = defineProps<{ def: OpDef }>();

const queue = useQueueStore();
const store = useDatVerifyStore();
const { liveRows } = storeToRefs(store);
const statusFilter = ref<string>("all");
const detailRow = ref<DatMatchData | null>(null);

void store.ensureRowListener();

function mine(job: QueueJob): boolean {
	return job.resultKind === "datVerify" && job.routeBack?.storeId === props.def.storeId;
}

const activeJob = computed(() =>
	queue.jobs.find((j) => mine(j) && (j.status === "queued" || j.status === "running")),
);

watch(
	() => queue.jobs.find((j) => mine(j) && j.status === "running")?.id,
	(id) => {
		if (id) liveRows.value.clear();
	},
);

// A directory input settles as one DatVerifyData holding every row; a file
// input settles as the single DatMatchData itself. While a directory job runs
// its rows stream in on the op's progress key instead.
const results = computed<DatMatchData[]>(() => {
	const out: DatMatchData[] = [];
	for (const job of queue.finished) {
		if (!mine(job)) continue;
		if (job.status !== "done" || !job.result || typeof job.result === "string") continue;
		const data = job.result.data as DatVerifyData | DatMatchData | null;
		if (!data) continue;
		if ("rows" in data) out.push(...data.rows);
		else out.push(data);
	}
	if (activeJob.value) out.push(...liveRows.value.values());
	return out;
});

const CHIPS: { verdict: string; label: string; color: "green" | "yellow" | "neutral" | "red" }[] = [
	{ verdict: "verified", label: "Verified", color: "green" },
	{ verdict: "hint", label: "Hint", color: "yellow" },
	{ verdict: "failed", label: "Failed", color: "red" },
	{ verdict: "unknown", label: "Unknown", color: "neutral" },
	{ verdict: "unsupported", label: "Unsupported", color: "neutral" },
];

const TAG: Record<string, { tag: string; label: string }> = {
	verified: { tag: "VERIFIED", label: "Verified" },
	hint: { tag: "HINT", label: "Hint" },
	unknown: { tag: "UNKNOWN", label: "Unknown" },
	unsupported: { tag: "UNSUPPORTED", label: "Unsupported" },
	failed: { tag: "FAILED", label: "Failed" },
};

const { counts, visibleRows, toggleFilter } = useResultRows(results, (r) => r.verdict, statusFilter);

function detail(r: DatMatchData): { text: string; tone: "green" | "red" | "muted" } | null {
	if (r.verdict === "failed") return { text: r.error ?? "Hash differs from the database entry.", tone: "red" };
	const text = [r.game_name, r.dat_file].filter(Boolean).join(" · ");
	if (!text) return null;
	return { text, tone: r.verdict === "verified" ? "green" : "muted" };
}

function detailLines(r: DatMatchData): string[] {
	const lines: string[] = [];
	if (r.game_name) lines.push(`Game: ${r.game_name}`);
	if (r.dat_file) lines.push(`DAT file: ${r.dat_file}`);
	lines.push(r.error ?? "The full hash does not match the database entry. The file may be modified or corrupt.");
	return lines;
}

function contextItems(r: DatMatchData) {
	return rowContextItems(r.path, detail(r)?.text);
}
</script>

<template>
	<ConfigCard v-if="results.length" title="Results">
		<div class="rc-chips">
			<FilterChip label="All" :count="results.length" :active="statusFilter === 'all'" @click="statusFilter = 'all'" />
			<FilterChip
				v-for="chip in CHIPS"
				:key="chip.verdict"
				:label="chip.label"
				:count="counts[chip.verdict] ?? 0"
				:color="chip.color"
				:active="statusFilter === chip.verdict"
				@click="toggleFilter(chip.verdict)"
			/>
		</div>

		<div
			v-for="r in visibleRows"
			:key="r.path"
			class="rc-row"
			:class="{ 'rc-row--fail': r.verdict === 'failed' }"
			@contextmenu="openContextMenu($event, contextItems(r))"
		>
			<StatusTag :status="TAG[r.verdict]?.tag ?? r.verdict" :label="TAG[r.verdict]?.label" />
			<div class="rc-row__text">
				<span class="rc-row__name">{{ basename(r.path) }}</span>
				<span
					v-if="detail(r)"
					class="rc-row__detail"
					:class="`rc-row__detail--${detail(r)!.tone}`"
				>{{ detail(r)!.text }}</span>
			</div>
			<button v-if="r.verdict === 'failed'" type="button" class="rc-link" @click="detailRow = r">Details</button>
		</div>
	</ConfigCard>

	<DetailModal
		v-if="detailRow"
		:title="basename(detailRow.path)"
		:lines="detailLines(detailRow)"
		@close="detailRow = null"
	/>
</template>

<style scoped>
.rc-chips {
	display: flex;
	flex-wrap: wrap;
	gap: 8px;
	padding-bottom: 4px;
}

.rc-row {
	display: flex;
	align-items: center;
	gap: 12px;
	padding: 8px 0;
	border-top: 1px solid var(--a06);
	user-select: text;
}

.rc-row--fail {
	background: rgba(212, 58, 62, 0.06);
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

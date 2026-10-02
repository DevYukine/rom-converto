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

const { counts, visibleRows: statusRows, toggleFilter } = useResultRows(results, (r) => r.verdict, statusFilter);
const query = ref("");
const visibleRows = computed(() => {
	const q = query.value.trim().toLowerCase();
	return q ? statusRows.value.filter((r) => r.path.toLowerCase().includes(q) || r.game_name?.toLowerCase().includes(q)) : statusRows.value;
});

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
	<ConfigCard v-if="results.length" title="Results" class="rc-results">
		<div class="rc-results__toolbar">
			<div class="rc-results__chips">
				<FilterChip label="All" :count="results.length" :active="statusFilter === 'all'" @click="statusFilter = 'all'" />
				<FilterChip
					v-for="chip in CHIPS"
					:key="chip.verdict"
					:label="chip.label"
					:count="counts[chip.verdict] ?? 0"
					:color="chip.color"
					:active="statusFilter === chip.verdict"
					:class="{ 'rc-results__chip--empty': !(counts[chip.verdict] ?? 0) }"
					@click="toggleFilter(chip.verdict)"
				/>
			</div>
			<input v-model="query" type="search" class="rc-input rc-results__search" placeholder="Filter by name" aria-label="Filter results by name">
		</div>
		<p class="rc-results__summary">{{ visibleRows.length }} of {{ results.length }} {{ results.length === 1 ? "file" : "files" }}</p>
		<div class="rc-results__columns" aria-hidden="true">
			<span>Status</span>
			<span>Name</span>
			<span class="rc-results__actions" />
		</div>
		<div v-if="!visibleRows.length" class="rc-results__none">Nothing matches this filter.</div>

		<div
			v-for="r in visibleRows"
			:key="r.path"
			class="rc-results__row"
			:class="{ 'rc-results__row--fail': r.verdict === 'failed', 'rc-results__row--multiline': !!detail(r) }"
			@contextmenu="openContextMenu($event, contextItems(r))"
		>
			<StatusTag :status="TAG[r.verdict]?.tag ?? r.verdict" :label="TAG[r.verdict]?.label" />
			<div class="rc-results__text">
				<span class="rc-results__name" :title="r.path">{{ basename(r.path) }}</span>
				<span
					v-if="detail(r)"
					class="rc-results__detail"
					:class="`rc-results__detail--${detail(r)!.tone}`"
					:title="detail(r)!.text"
				>{{ detail(r)!.text }}</span>
			</div>
			<div class="rc-results__actions">
				<button v-if="r.verdict === 'failed'" type="button" class="rc-results__link" @click="detailRow = r">Details</button>
			</div>
		</div>
	</ConfigCard>

	<DetailModal
		v-if="detailRow"
		:title="basename(detailRow.path)"
		:lines="detailLines(detailRow)"
		tone="error"
		@close="detailRow = null"
	/>
</template>


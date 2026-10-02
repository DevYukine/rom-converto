<script setup lang="ts">
import { computed } from "vue";
import { useQueueStore, type QueueJob } from "~/stores/queue";
import { basename } from "~/composables/useDerivedPath";
import { digestValues } from "~/lib/display";
import { requestPath } from "~/lib/opdefs/types";
import ConfigCard from "~/components/ui/ConfigCard.vue";

interface HashRow {
	key: string;
	name: string;
	values: { label: string; value: string }[];
}

const queue = useQueueStore();

// A recursive run answers with one row per file; a single file answers with
// its digests alone, named by the path the job was given.
function jobRows(job: QueueJob): { path: string; digests: unknown }[] {
	const data = typeof job.result === "string" ? null : job.result?.data;
	if (Array.isArray(data)) return data as { path: string; digests: unknown }[];
	if (!data) return [];
	return [{ path: requestPath(job.args, "input") || job.name, digests: data }];
}

const rows = computed<HashRow[]>(() => {
	const jobs = queue.finished.filter((j) => j.resultKind === "hash" && j.status === "done");
	const out: HashRow[] = [];
	for (const job of jobs.slice().reverse()) {
		for (const row of jobRows(job)) {
			out.push({ key: row.path, name: basename(row.path), values: digestValues(row.digests) });
		}
	}
	return out;
});

const { show: showToast } = useToast();

async function copy(value: string) {
	try {
		await navigator.clipboard.writeText(value);
	} catch {
		// clipboard unavailable (permission denied or no secure context); nothing to fall back to.
	}
	showToast("Copied");
}
</script>

<template>
	<ConfigCard :title="`Hashes · ${rows.length} file${rows.length === 1 ? '' : 's'}`">
		<p v-if="rows.length === 0" class="rc-hash__empty">
			No hashes yet. Stage files and add them to the queue; results appear here as jobs finish.
		</p>
		<div v-for="row in rows" :key="row.key" class="rc-hash__row">
			<span class="rc-hash__name" :title="row.name">{{ row.name }}</span>
			<div class="rc-hash__grid">
				<template v-for="entry in row.values" :key="entry.label">
					<span class="rc-hash__label">{{ entry.label }}</span>
					<button type="button" class="rc-hash__value" :title="`Copy ${entry.label}`" @click="copy(entry.value)">{{ entry.value }}</button>
				</template>
			</div>
		</div>
	</ConfigCard>
</template>

<style scoped>
.rc-hash__empty {
	margin: 0;
	font-size: var(--fs-sm);
	color: var(--t5);
	line-height: var(--lh-body);
}

.rc-hash__row {
	padding: 8px 0;
	border-top: 1px solid var(--a06);
}

.rc-hash__row:first-child {
	border-top: none;
}

.rc-hash__name {
	display: block;
	font-size: var(--fs-md);
	color: var(--t0);
	margin-bottom: 4px;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-hash__grid {
	display: grid;
	grid-template-columns: auto minmax(0, 1fr);
	gap: 2px 10px;
}

.rc-hash__label {
	font-size: var(--fs-sm);
	color: var(--t5);
}

.rc-hash__value {
	min-width: 0;
	max-width: 100%;
	font-family: var(--font-mono);
	font-size: var(--fs-sm);
	overflow-wrap: anywhere;
	color: var(--t3);
	background: none;
	border: none;
	padding: 0;
	text-align: left;
	cursor: pointer;
	justify-self: start;
}

.rc-hash__value:hover {
	color: var(--blue);
}
</style>

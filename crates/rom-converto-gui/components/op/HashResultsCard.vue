<script setup lang="ts">
import { computed } from "vue";
import { useQueueStore } from "~/stores/queue";
import { basename } from "~/composables/useDerivedPath";
import { parseHashLine } from "~/lib/hash-lines";
import ConfigCard from "~/components/ui/ConfigCard.vue";

interface HashRow {
	key: string;
	name: string;
	values: { label: string; value: string }[];
}

const queue = useQueueStore();

const rows = computed<HashRow[]>(() => {
	const jobs = queue.finished.filter((j) => j.resultKind === "hash" && j.status === "done");
	const out: HashRow[] = [];
	for (const job of jobs.slice().reverse()) {
		for (const line of String(job.result ?? "").split("\n")) {
			const row = parseHashLine(line);
			if (row) out.push({ key: row.path, name: basename(row.path), values: row.values });
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
	<ConfigCard :title="`Hashes · ${rows.length} files`">
		<p v-if="rows.length === 0" class="rc-hash__empty">
			No hashes yet. Stage files and add them to the queue; results appear here as jobs finish.
		</p>
		<div v-for="row in rows" :key="row.key" class="rc-hash__row">
			<span class="rc-hash__name">{{ row.name }}</span>
			<div class="rc-hash__grid">
				<template v-for="entry in row.values" :key="entry.label">
					<span class="rc-hash__label">{{ entry.label }}</span>
					<button type="button" class="rc-hash__value" @click="copy(entry.value)">{{ entry.value }}</button>
				</template>
			</div>
		</div>
	</ConfigCard>
</template>

<style scoped>
.rc-hash__empty {
	margin: 0;
	font-size: 11.5px;
	color: var(--t5);
	line-height: 1.5;
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
	font-size: 12px;
	color: var(--t0);
	margin-bottom: 4px;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-hash__grid {
	display: grid;
	grid-template-columns: auto 1fr;
	gap: 2px 10px;
}

.rc-hash__label {
	font-size: 10.5px;
	color: var(--t4);
}

.rc-hash__value {
	font-family: ui-monospace, monospace;
	font-size: 11px;
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

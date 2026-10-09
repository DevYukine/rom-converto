<script setup lang="ts">
import { computed, ref } from "vue";
import { useQueueStore, type QueueJob } from "~/stores/queue";
import { openContextMenu } from "~/composables/useContextMenu";
import { rowContextItems, useResultRows } from "~/composables/useResultRows";
import { requestPath } from "~/lib/opdefs/types";
import FilterChip from "~/components/ui/FilterChip.vue";
import ConfigCard from "~/components/ui/ConfigCard.vue";
import StatusTag from "~/components/ui/StatusTag.vue";
import DetailModal from "~/components/modals/DetailModal.vue";
import type { OpDef } from "~/lib/opdefs/types";
import { rvzStructureOk } from "~/lib/fields";

const props = defineProps<{ def: OpDef }>();

const queue = useQueueStore();

interface Row {
	job: QueueJob;
	ok: boolean;
	detail: string;
	lines: string[];
}

function summarize(data: Record<string, any>, console: string): { ok: boolean; detail: string; lines: string[] } {
	switch (console) {
		case "ctr": {
			if (data.format === "Cia") {
				const leg = typeof data.legitimacy === "string" ? data.legitimacy : Object.keys(data.legitimacy)[0];
				const ok = data.content_hashes_valid !== false;
				const hashes = data.content_hashes_valid == null ? "" : ` · content hashes ${data.content_hashes_valid ? "✓" : "✗"}`;
				return { ok, detail: `${leg} · title ${data.title_id}${hashes}`, lines: data.details ?? [] };
			}
			const bad = (data.partitions ?? []).filter((p: any) => !p.ncch_magic_valid).length;
			const ok = data.ncsd_magic_valid && bad === 0;
			return {
				ok,
				detail: `NCSD · ${data.partition_count} partition(s)${bad ? ` · ${bad} invalid` : ""}`,
				lines: data.details ?? [],
			};
		}
		case "dol": {
			const ok = !!data.ok;
			const parts: string[] = [];
			if (data.rvz_structure) parts.push(`RVZ structure ${rvzStructureOk(data.rvz_structure) ? "✓" : "✗"}`);
			if (data.disc_sha1) parts.push(`SHA-1 ${String(data.disc_sha1).slice(0, 12)}…`);
			const lines: string[] = [...(data.structural?.notes ?? [])];
			if (data.rvz_note) lines.unshift(data.rvz_note);
			return { ok, detail: parts.join(" · ") || (ok ? "structure ok" : "structure mismatch"), lines };
		}
		case "rvl": {
			const ok = !!data.ok;
			const partitions = data.partitions ?? [];
			const bad = partitions.reduce((n: number, p: any) => n + p.mismatched_clusters, 0);
			const parts: string[] = [];
			if (data.rvz_structure) parts.push(`RVZ structure ${rvzStructureOk(data.rvz_structure) ? "✓" : "✗"}`);
			parts.push(`${partitions.length} partition(s)${bad ? ` · ${bad} mismatched clusters` : ""}`);
			const lines: string[] = data.rvz_note ? [data.rvz_note] : [];
			lines.push(
				...partitions
					.filter((p: any) => !p.ok)
					.map((p: any) => p.note ?? `partition @0x${p.offset.toString(16)}: ${p.mismatched_clusters} mismatched clusters`),
			);
			return { ok, detail: parts.join(" · "), lines };
		}
		case "wup": {
			const ok = !!data.ok;
			const titles = data.titles ?? [];
			const mismatched = titles.reduce((n: number, t: any) => n + t.mismatched_content, 0);
			const detail = `${data.kind} · ${titles.length} title(s)${mismatched ? ` · ${mismatched} mismatched` : ""}`;
			const lines = titles.map(
				(t: any) => `${t.title_id_hex}: ${t.ok ? "ok" : "FAIL"} (verified ${t.verified_content}, mismatched ${t.mismatched_content}, skipped ${t.skipped_content})`,
			);
			return { ok, detail, lines };
		}
		case "nx": {
			const ok = !!data.ok;
			const ncas = data.ncas ?? [];
			const bad = ncas.filter((n: any) => !n.ok).length;
			const detail = `${data.kind} · ${ncas.length} NCA(s)${bad ? ` · ${bad} mismatch` : ""}`;
			const lines = ncas
				.filter((n: any) => !n.ok)
				.map((n: any) => `${n.name}${n.partition ? ` (${n.partition})` : ""}: ${n.mismatched_sections} section(s) mismatched`);
			return { ok, detail, lines };
		}
		case "chd": {
			const ok = data.ok !== false;
			const detail = ok ? "SHA-1 ok" : "SHA-1 mismatch";
			return { ok, detail, lines: [detail] };
		}
		case "cso": {
			const ok = data.ok !== false;
			const mismatches = typeof data.mismatches === "number" ? ` (${data.mismatches})` : "";
			const detail = ok ? "structure ok" : `structure mismatch${mismatches}`;
			return { ok, detail, lines: [detail] };
		}
		case "xenon": {
			const ok = data.hash_ok !== false;
			const detail = `${data.blocks} block(s) · ${ok ? "hashes ok" : "hash mismatch"}`;
			return { ok, detail, lines: [detail] };
		}
		default:
			return { ok: true, detail: "", lines: [] };
	}
}

function resultData(job: QueueJob): Record<string, any> | null {
	if (typeof job.result === "string") return null;
	return (job.result?.data as Record<string, any> | undefined) ?? null;
}

function toRow(job: QueueJob): Row {
	if (job.status === "failed") {
		const msg = job.error ?? "Verification failed.";
		return { job, ok: false, detail: msg, lines: [msg] };
	}
	const data = resultData(job);
	if (!data) return { job, ok: true, detail: "", lines: [] };
	const { ok, detail, lines } = summarize(data, props.def.console);
	return { job, ok, detail, lines };
}

const rows = computed<Row[]>(() => {
	const out: Row[] = [];
	for (const job of queue.finished) {
		if (job.resultKind !== "verify") continue;
		if (job.routeBack?.storeId !== props.def.storeId) continue;
		if (job.status !== "done" && job.status !== "failed") continue;
		out.push(toRow(job));
	}
	return out.reverse();
});

const statusFilter = ref<"passed" | "failed" | "all">("all");
const { counts, visibleRows: statusRows, toggleFilter } = useResultRows(rows, (r) => (r.ok ? "passed" : "failed"), statusFilter);
const query = ref("");
const visibleRows = computed(() => {
	const q = query.value.trim().toLowerCase();
	return q ? statusRows.value.filter((r) => r.job.name.toLowerCase().includes(q)) : statusRows.value;
});

const detailRow = ref<Row | null>(null);
</script>

<template>
	<ConfigCard v-if="rows.length" title="Results" class="rc-results">
		<div class="rc-results__toolbar">
			<div class="rc-results__chips">
				<FilterChip label="All" :count="rows.length" :active="statusFilter === 'all'" @click="statusFilter = 'all'" />
				<FilterChip label="Passed" :count="counts.passed ?? 0" color="green" :active="statusFilter === 'passed'" :class="{ 'rc-results__chip--empty': !counts.passed }" @click="toggleFilter('passed')" />
				<FilterChip label="Failed" :count="counts.failed ?? 0" color="red" :active="statusFilter === 'failed'" :class="{ 'rc-results__chip--empty': !counts.failed }" @click="toggleFilter('failed')" />
			</div>
			<input v-model="query" type="search" class="rc-input rc-results__search" placeholder="Filter by name" aria-label="Filter results by name">
		</div>
		<p class="rc-results__summary">{{ counts.passed ?? 0 }} passed · {{ counts.failed ?? 0 }} failed · {{ visibleRows.length }} of {{ rows.length }} {{ rows.length === 1 ? "file" : "files" }}</p>
		<div class="rc-results__columns" aria-hidden="true">
			<span>Status</span>
			<span>Name</span>
			<span class="rc-results__actions" />
		</div>
		<div v-if="!visibleRows.length" class="rc-results__none">Nothing matches this filter.</div>
		<div
			v-for="row in visibleRows"
			:key="row.job.id"
			class="rc-results__row"
			:class="{ 'rc-results__row--fail': !row.ok, 'rc-results__row--multiline': !!row.detail }"
			@contextmenu="openContextMenu($event, rowContextItems(requestPath(row.job.args, 'input') || row.job.name, row.detail))"
		>
			<StatusTag :status="row.ok ? 'PASSED' : 'FAILED'" :label="row.ok ? 'Passed' : 'Failed'" />
			<div class="rc-results__text">
				<span class="rc-results__name" :title="row.job.name">{{ row.job.name }}</span>
				<span v-if="row.detail" class="rc-results__detail" :class="{ 'rc-results__detail--red': !row.ok }" :title="row.detail">
					{{ row.detail }}
				</span>
			</div>
			<div class="rc-results__actions">
				<button v-if="row.lines.length" type="button" class="rc-results__link" @click="detailRow = row">Details</button>
			</div>
		</div>
	</ConfigCard>

	<DetailModal
		v-if="detailRow"
		:title="detailRow.job.name"
		:lines="detailRow.lines"
		:tone="detailRow.ok ? 'plain' : 'error'"
		@close="detailRow = null"
	/>
</template>


<style scoped>
.rc-results__detail {
	white-space: normal;
	overflow-wrap: anywhere;
	line-height: var(--lh-body);
	text-wrap: pretty;
}
</style>

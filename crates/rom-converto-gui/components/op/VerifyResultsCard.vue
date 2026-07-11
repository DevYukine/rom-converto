<script setup lang="ts">
import { computed, ref } from "vue";
import { useQueueStore, type QueueJob } from "~/stores/queue";
import ConfigCard from "~/components/ui/ConfigCard.vue";
import StatusTag from "~/components/ui/StatusTag.vue";
import DetailModal from "~/components/modals/DetailModal.vue";
import type { OpDef } from "~/lib/opdefs/types";

const props = defineProps<{ def: OpDef }>();

const queue = useQueueStore();

interface Row {
	job: QueueJob;
	ok: boolean;
	detail: string;
	lines: string[];
}

function summarize(data: Record<string, any>, command: string): { ok: boolean; detail: string; lines: string[] } {
	switch (command) {
		case "cmd_verify_ctr": {
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
		case "cmd_verify_dol": {
			const ok = !!data.ok;
			const parts: string[] = [];
			if (data.rvz_structure) parts.push(`RVZ structure ${data.rvz_structure.ok ? "✓" : "✗"}`);
			if (data.disc_sha1) parts.push(`SHA-1 ${String(data.disc_sha1).slice(0, 12)}…`);
			return { ok, detail: parts.join(" · ") || (ok ? "structure ok" : "structure mismatch"), lines: data.structural?.notes ?? [] };
		}
		case "cmd_verify_rvl": {
			const ok = !!data.ok;
			const partitions = data.partitions ?? [];
			const bad = partitions.reduce((n: number, p: any) => n + p.mismatched_clusters, 0);
			const parts: string[] = [];
			if (data.rvz_structure) parts.push(`RVZ structure ${data.rvz_structure.ok ? "✓" : "✗"}`);
			parts.push(`${partitions.length} partition(s)${bad ? ` · ${bad} mismatched clusters` : ""}`);
			const lines = partitions
				.filter((p: any) => !p.ok)
				.map((p: any) => p.note ?? `partition @0x${p.offset.toString(16)}: ${p.mismatched_clusters} mismatched clusters`);
			return { ok, detail: parts.join(" · "), lines };
		}
		case "cmd_wup_verify": {
			const ok = !!data.ok;
			const titles = data.titles ?? [];
			const mismatched = titles.reduce((n: number, t: any) => n + t.mismatched_content, 0);
			const detail = `${data.kind} · ${titles.length} title(s)${mismatched ? ` · ${mismatched} mismatched` : ""}`;
			const lines = titles.map(
				(t: any) => `${t.title_id_hex}: ${t.ok ? "ok" : "FAIL"} (verified ${t.verified_content}, mismatched ${t.mismatched_content}, skipped ${t.skipped_content})`,
			);
			return { ok, detail, lines };
		}
		case "cmd_nx_verify": {
			const ok = !!data.ok;
			const ncas = data.ncas ?? [];
			const bad = ncas.filter((n: any) => !n.ok).length;
			const detail = `${data.kind} · ${ncas.length} NCA(s)${bad ? ` · ${bad} mismatch` : ""}`;
			const lines = ncas
				.filter((n: any) => !n.ok)
				.map((n: any) => `${n.name}${n.partition ? ` (${n.partition})` : ""}: ${n.mismatched_sections} section(s) mismatched`);
			return { ok, detail, lines };
		}
		case "cmd_chd_verify": {
			const ok = data.ok !== false;
			const detail = ok ? "SHA-1 ok" : "SHA-1 mismatch";
			return { ok, detail, lines: [detail] };
		}
		case "cmd_cso_verify": {
			const ok = data.ok !== false;
			const mismatches = typeof data.mismatches === "number" ? ` (${data.mismatches})` : "";
			const detail = ok ? "structure ok" : `structure mismatch${mismatches}`;
			return { ok, detail, lines: [detail] };
		}
		default:
			return { ok: true, detail: "", lines: [] };
	}
}

function toRow(job: QueueJob): Row {
	if (job.status === "failed") {
		const msg = job.error ?? "Verification failed.";
		return { job, ok: false, detail: msg, lines: [msg] };
	}
	let data: Record<string, any> | null = null;
	if (typeof job.result === "string") {
		try {
			data = JSON.parse(job.result);
		} catch {
			data = null;
		}
	}
	if (!data) return { job, ok: true, detail: "", lines: [] };
	const { ok, detail, lines } = summarize(data, job.command);
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

const passedCount = computed(() => rows.value.filter((r) => r.ok).length);
const failedCount = computed(() => rows.value.filter((r) => !r.ok).length);

const detailRow = ref<Row | null>(null);
</script>

<template>
	<ConfigCard v-if="rows.length" title="Results">
		<template #head-tag>
			<span class="rc-counts">
				<strong class="rc-counts__pass">{{ passedCount }} passed</strong>
				<span class="rc-counts__dot">·</span>
				<strong class="rc-counts__fail">{{ failedCount }} failed</strong>
			</span>
		</template>

		<div v-for="row in rows" :key="row.job.id" class="rc-row" :class="{ 'rc-row--fail': !row.ok }">
			<StatusTag :status="row.ok ? 'PASSED' : 'FAILED'" />
			<div class="rc-row__text">
				<span class="rc-row__name">{{ row.job.name }}</span>
				<span v-if="row.detail" class="rc-row__detail" :class="row.ok ? 'rc-row__detail--pass' : 'rc-row__detail--fail'">
					{{ row.detail }}
				</span>
			</div>
			<button v-if="row.lines.length" type="button" class="rc-link" @click="detailRow = row">Details</button>
		</div>
	</ConfigCard>

	<DetailModal
		v-if="detailRow"
		:title="detailRow.job.name"
		:lines="detailRow.lines"
		@close="detailRow = null"
	/>
</template>

<style scoped>
.rc-counts {
	display: flex;
	align-items: center;
	gap: 6px;
	font-size: 11.5px;
}

.rc-counts__pass {
	color: var(--green);
}

.rc-counts__fail {
	color: var(--red);
}

.rc-counts__dot {
	color: var(--t5);
}

.rc-row {
	display: flex;
	align-items: center;
	gap: 12px;
	padding: 8px 0;
	border-top: 1px solid var(--a06);
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

.rc-row__detail--pass {
	color: var(--t4);
}

.rc-row__detail--fail {
	color: var(--red);
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

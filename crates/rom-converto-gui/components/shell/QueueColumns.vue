<script setup lang="ts">
import { useQueueStore, type QueueJob } from "~/stores/queue";
import { useProgress } from "~/composables/useProgress";

defineProps<{ full?: boolean }>();

const queue = useQueueStore();

function pct(job: QueueJob) {
	return useProgress(job.taskId).percent.value;
}
function jobSpeed(job: QueueJob) {
	const p = useProgress(job.taskId);
	return p.running.value ? (p.current.value / 1e6).toFixed(0) : "0";
}

const dragId = ref<string | null>(null);
function onDrop(targetId: string) {
	const ids = queue.queued.map((j) => j.id);
	const from = ids.indexOf(dragId.value ?? "");
	const to = ids.indexOf(targetId);
	if (from < 0 || to < 0) return;
	const [moved] = ids.splice(from, 1);
	if (moved === undefined) return;
	ids.splice(to, 0, moved);
	queue.reorder(ids);
	dragId.value = null;
}

function mark(status: string) {
	if (status === "done") return { ch: "✓", cls: "m-done" };
	if (status === "failed") return { ch: "✕", cls: "m-fail" };
	return { ch: "–", cls: "m-cancel" };
}

function resultText(job: QueueJob): string {
	if (job.status === "failed") return job.error ?? "failed";
	if (job.status === "cancelled") return "cancelled";
	if (job.outputBytes > 0 && job.inputBytes > 0) {
		return `-${(100 - (job.outputBytes / job.inputBytes) * 100).toFixed(1)}%`;
	}
	return "done";
}
</script>

<template>
	<div class="body" :class="{ full }">
		<div class="col run">
			<div class="colhead">Running</div>
			<div v-if="!queue.running.length" class="empty">Nothing running.</div>
			<div v-for="job in queue.running" :key="job.id" class="rcard">
				<svg class="ring" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#4593f8" stroke-width="3" stroke-linecap="round">
					<path d="M21 12a9 9 0 1 1-6.2-8.5" />
				</svg>
				<div class="rmain">
					<div class="rtop">
						<span class="name">{{ job.name }}</span>
						<span class="stat">{{ pct(job) }}% · {{ jobSpeed(job) }} MB/s</span>
						<button class="cancel" @click="queue.cancel(job.id)">Cancel</button>
					</div>
					<span class="bar"><span class="fill" :style="{ width: pct(job) + '%' }" /></span>
					<span class="locked">{{ job.chips }} · locked at queue time</span>
				</div>
			</div>
		</div>

		<div class="col next">
			<div class="colhead">Up next<span class="sub">drag to reorder</span></div>
			<div v-if="!queue.queued.length" class="empty">Queue is empty. Add jobs from any operation page.</div>
			<div
				v-for="job in queue.queued"
				:key="job.id"
				class="nrow"
				draggable="true"
				@dragstart="dragId = job.id"
				@dragover.prevent
				@drop="onDrop(job.id)"
			>
				<span class="grip">⠿</span>
				<span class="name">{{ job.name }}</span>
				<span class="optag">{{ job.opLabel }}</span>
				<button class="rm" @click="queue.remove(job.id)">Remove</button>
			</div>
		</div>

		<div class="col fin">
			<div class="colhead">Finished this session<span class="sub green">{{ queue.savedGiB }} GiB saved</span></div>
			<div v-for="job in queue.finished" :key="job.id" class="frow">
				<span :class="['fmark', mark(job.status).cls]">{{ mark(job.status).ch }}</span>
				<span class="name">{{ job.name }}</span>
				<span class="optag">{{ job.opLabel }}</span>
				<span :class="['fres', mark(job.status).cls]" :title="job.status === 'failed' ? job.error : undefined">{{ resultText(job) }}</span>
				<button v-if="job.status === 'failed'" class="retry" @click="queue.retry(job.id)">Retry</button>
			</div>
		</div>
	</div>
</template>

<style scoped>
.body {
	display: flex;
	height: min(236px, 32vh);
}
.body.full {
	flex: 1;
	min-height: 0;
	height: auto;
}
.col {
	display: flex;
	flex-direction: column;
	gap: 6px;
	padding: 8px 12px;
	overflow-y: auto;
}
.col.run {
	flex: 1.2;
}
.col.next,
.col.fin {
	flex: 1;
	border-left: 1px solid var(--a05);
}
.colhead {
	display: flex;
	justify-content: space-between;
	font-size: 10.5px;
	font-weight: 700;
	text-transform: uppercase;
	letter-spacing: 0.8px;
	color: var(--t4);
	margin-bottom: 2px;
}
.sub {
	font-weight: 400;
	text-transform: none;
	letter-spacing: 0;
	color: var(--t6);
}
.sub.green {
	color: var(--green);
}
.empty {
	color: var(--t6);
	font-size: 12px;
}
.rcard {
	display: flex;
	gap: 8px;
	background: var(--a03);
	border-radius: 8px;
	padding: 8px 10px;
}
.ring {
	flex-shrink: 0;
	margin-top: 2px;
	animation: rcspin 1s linear infinite;
}
@keyframes rcspin {
	to {
		transform: rotate(360deg);
	}
}
.rmain {
	flex: 1;
	min-width: 0;
	display: flex;
	flex-direction: column;
	gap: 5px;
}
.rtop {
	display: flex;
	align-items: center;
	gap: 8px;
	font-size: 12px;
}
.rtop .name {
	flex: 1;
	min-width: 0;
	color: var(--t1);
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}
.stat {
	font-family: ui-monospace, monospace;
	color: var(--blue);
}
.cancel {
	background: none;
	border: none;
	color: var(--red);
	cursor: pointer;
	font-size: 11.5px;
}
.bar {
	height: 6px;
	border-radius: 3px;
	background: var(--a10);
	overflow: hidden;
}
.fill {
	display: block;
	height: 100%;
	background: #3b82f6;
	transition: width 0.4s;
}
.locked {
	font-family: ui-monospace, monospace;
	font-size: 9.5px;
	color: var(--t5);
}
.nrow,
.frow {
	display: flex;
	align-items: center;
	gap: 8px;
	font-size: 12px;
}
.nrow {
	cursor: grab;
}
.grip {
	color: var(--t7);
}
.nrow .name,
.frow .name {
	flex: 1;
	min-width: 64px;
	color: var(--t2);
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}
.optag {
	font-family: ui-monospace, monospace;
	font-size: 10px;
	color: var(--t5);
	white-space: nowrap;
	max-width: 96px;
	overflow: hidden;
	text-overflow: ellipsis;
}
.rm,
.retry {
	background: none;
	border: none;
	cursor: pointer;
	font-size: 11.5px;
}
.rm {
	color: var(--t5);
}
.rm:hover {
	color: var(--red);
}
.retry {
	color: var(--blue);
	text-decoration: underline;
}
.fmark {
	width: 12px;
	text-align: center;
}
.fres {
	max-width: 40%;
	font-family: ui-monospace, monospace;
	font-size: 10px;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}
.m-done {
	color: var(--green);
}
.m-fail {
	color: var(--red);
}
.m-cancel {
	color: var(--yellow2);
}
</style>

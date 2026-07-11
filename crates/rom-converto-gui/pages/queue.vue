<script setup lang="ts">
import { useQueueStore } from "~/stores/queue";
import { useUiStore } from "~/stores/ui";
import { useJobConcurrency } from "~/composables/useJobConcurrency";

const queue = useQueueStore();
const ui = useUiStore();
const { concurrency, maxConcurrency } = useJobConcurrency();

const showStartPause = computed(
	() => !ui.startImmediately && (queue.queued.length > 0 || queue.queueActive),
);

function stepConcurrency(delta: number) {
	concurrency.value = Math.min(maxConcurrency, Math.max(1, concurrency.value + delta));
	queue.pump();
}
</script>

<template>
	<div class="page">
		<div class="header">
			<div>
				<h1>Global queue</h1>
				<p>Every job from every page runs through this one queue. Parameters are locked per job.</p>
			</div>
			<div class="toolbar">
				<span class="stepper">
					Concurrent jobs
					<button @click="stepConcurrency(-1)">−</button>
					{{ concurrency }}
					<button @click="stepConcurrency(1)">+</button>
				</span>
				<button v-if="showStartPause" class="btn primary" @click="queue.queueActive ? queue.pause() : queue.start()">
					{{ queue.queueActive ? "Pause" : "Start" }}
				</button>
				<button class="btn out" @click="queue.retryFailed()">Retry failed</button>
				<button class="btn out" @click="queue.clearFinished()">Clear finished</button>
				<button class="btn danger" @click="queue.cancelAll()">Cancel all</button>
			</div>
		</div>

		<QueueColumns full />
	</div>
</template>

<style scoped>
.page {
	display: flex;
	flex-direction: column;
	height: 100%;
	min-height: 0;
	padding: 20px 26px;
}
.header {
	display: flex;
	align-items: flex-start;
	justify-content: space-between;
	gap: 16px;
	margin-bottom: 14px;
}
h1 {
	font-size: 18px;
	font-weight: 700;
	color: var(--t0);
}
.header p {
	font-size: 11.5px;
	color: var(--t4);
	margin-top: 4px;
}
.toolbar {
	display: flex;
	align-items: center;
	gap: 10px;
}
.stepper {
	color: var(--t4);
	font-size: 11px;
	display: flex;
	align-items: center;
	gap: 6px;
}
.stepper button {
	background: transparent;
	border: none;
	color: var(--t4);
	cursor: pointer;
	font-size: 13px;
}
.stepper button:hover {
	color: var(--t0);
}
.btn {
	border-radius: 7px;
	padding: 6px 14px;
	font-size: 12px;
	font-weight: 600;
	cursor: pointer;
}
.btn.primary {
	background: #2f6fd0;
	color: #fff;
	border: none;
}
.btn.primary:hover {
	background: #3b82f6;
}
.btn.out {
	background: transparent;
	color: var(--t3);
	border: 1px solid var(--a16);
}
.btn.danger {
	background: #d43a3e;
	color: #fff;
	border: none;
}
.btn.danger:hover {
	background: #e04a4e;
}
</style>

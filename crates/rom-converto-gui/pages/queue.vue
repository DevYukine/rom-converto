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

function setConcurrency(value: number) {
	concurrency.value = value;
	queue.pump();
}
</script>

<template>
	<div class="page rc-page">
		<div class="header">
			<div>
				<h1>Global queue</h1>
				<p>Every job from every page runs through this one queue. Parameters are locked per job.</p>
			</div>
			<div class="toolbar">
				<span class="stepper">
					<span>Concurrent jobs</span>
					<Stepper :model-value="concurrency" :min="1" :max="maxConcurrency" label="Concurrent jobs" @update:model-value="setConcurrency" />
				</span>
				<button v-if="showStartPause" class="btn primary" @click="queue.queueActive ? queue.pause() : queue.start()">
					{{ queue.queueActive ? "Pause" : "Start" }}
				</button>
				<button class="btn out" :disabled="!queue.failed.length" @click="queue.retryFailed()">Retry failed</button>
				<button class="btn out" :disabled="!queue.finished.length" @click="queue.clearFinished()">Clear finished</button>
				<button class="btn danger" :class="{ active: queue.running.length > 0 || queue.queued.length > 0 }" :disabled="!queue.running.length && !queue.queued.length" @click="queue.cancelAll()">Cancel all</button>
			</div>
		</div>

		<QueueColumns full />
	</div>
</template>

<style scoped>
.page {
	display: flex;
	flex-direction: column;
	min-height: 100%;
	max-width: none;
	padding: 24px 28px 32px;
}
.header {
	display: flex;
	flex-wrap: wrap;
	align-items: flex-start;
	justify-content: space-between;
	gap: 16px 24px;
	margin-bottom: 16px;
}
.header > div:first-child {
	flex: 1 1 320px;
	min-width: 0;
}
h1 {
	font-size: var(--fs-xl);
	line-height: 1.25;
	text-wrap: balance;
	font-weight: 700;
	color: var(--t0);
}
.header p {
	font-size: var(--fs-md);
	color: var(--t4);
	margin-top: 4px;
	max-width: 72ch;
	line-height: var(--lh-body);
	text-wrap: pretty;
}
.toolbar {
	display: flex;
	flex-wrap: wrap;
	align-items: center;
	gap: 10px;
}
.stepper {
	display: inline-flex;
	align-items: center;
	gap: 8px;
	color: var(--t4);
	font-size: var(--fs-sm);
	white-space: nowrap;
}
.btn {
	flex: none;
	height: 32px;
	border-radius: var(--r-md);
	padding: 0 14px;
	font-size: var(--fs-md);
	font-weight: 600;
	cursor: pointer;
	white-space: nowrap;
}
.btn.primary {
	background: var(--fill);
	color: #fff;
	border: none;
}
.btn.primary:hover:not(:disabled) {
	background: var(--fill-hover);
}
.btn.out {
	background: transparent;
	color: var(--t3);
	border: 1px solid var(--a16);
}
.btn.out:hover:not(:disabled) {
	border-color: var(--a40);
	background: var(--a04);
}
.btn.danger {
	background: transparent;
	color: var(--red);
	border: 1px solid var(--red);
}
.btn.danger:hover:not(:disabled) {
	background: var(--tint-red);
}
.btn.danger.active {
	background: var(--fill-danger);
	color: #fff;
}
.btn.danger.active:hover {
	filter: brightness(1.1);
}
.btn:disabled {
	color: var(--t5);
	border-color: var(--a10);
	cursor: not-allowed;
}
</style>

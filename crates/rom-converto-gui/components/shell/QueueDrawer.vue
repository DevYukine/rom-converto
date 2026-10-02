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
	<div class="drawer">
		<div class="head">
			<span class="title">Global queue</span>
			<span class="spacer" />
			<button v-if="showStartPause" class="btn primary" @click="queue.queueActive ? queue.pause() : queue.start()">
				{{ queue.queueActive ? "Pause" : "Start" }}
			</button>
			<button class="btn out" :disabled="!queue.failed.length" @click="queue.retryFailed()">Retry failed</button>
			<button class="btn out" :disabled="!queue.finished.length" @click="queue.clearFinished()">Clear finished</button>
			<span class="stepper">
				<span>Concurrent jobs</span>
				<Stepper :model-value="concurrency" :min="1" :max="maxConcurrency" label="Concurrent jobs" @update:model-value="setConcurrency" />
			</span>
		</div>

		<QueueColumns />
	</div>
</template>

<style scoped>
.drawer {
	display: flex;
	flex-direction: column;
	flex-shrink: 0;
	background: var(--bg2);
	border-top: 1px solid var(--a09);
	container: queue / inline-size;
	height: clamp(200px, 32vh, 480px);
	overflow: hidden;
}
.head {
	display: flex;
	flex: none;
	align-items: center;
	flex-wrap: wrap;
	gap: 8px 12px;
	padding: 10px 16px;
	font-size: var(--fs-sm);
}
.title {
	font-size: var(--fs-lg);
	font-weight: 600;
	color: var(--t0);
}
.spacer {
	flex: 1;
}
.btn {
	flex: none;
	height: 32px;
	border-radius: var(--r-md);
	padding: 0 12px;
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
.btn:disabled {
	color: var(--t5);
	border-color: var(--a10);
	cursor: not-allowed;
}
.stepper {
	display: inline-flex;
	align-items: center;
	gap: 8px;
	color: var(--t4);
	font-size: var(--fs-sm);
	white-space: nowrap;
}
</style>

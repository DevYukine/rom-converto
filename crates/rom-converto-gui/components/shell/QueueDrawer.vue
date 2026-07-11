<script setup lang="ts">
import { useQueueStore } from "~/stores/queue";
import { useUiStore } from "~/stores/ui";
import { useJobConcurrency } from "~/composables/useJobConcurrency";

const props = defineProps<{ hideHeader?: boolean }>();

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
	<div class="drawer">
		<div v-if="!props.hideHeader" class="head">
			<span class="title">Global queue</span>
			<span class="counts">
				<b class="c-run">{{ queue.counts.running }} running</b> ·
				{{ queue.counts.queued }} queued ·
				<b class="c-done">{{ queue.counts.done }} done</b> ·
				<b class="c-fail">{{ queue.counts.failed }} failed</b>
			</span>
			<span class="spacer" />
			<button v-if="showStartPause" class="btn primary" @click="queue.queueActive ? queue.pause() : queue.start()">
				{{ queue.queueActive ? "Pause" : "Start" }}
			</button>
			<button class="btn out" @click="queue.retryFailed()">Retry failed</button>
			<button class="btn out" @click="queue.clearFinished()">Clear finished</button>
			<span class="stepper">
				Concurrent jobs
				<button @click="stepConcurrency(-1)">−</button>
				{{ concurrency }}
				<button @click="stepConcurrency(1)">+</button>
			</span>
		</div>

		<QueueColumns />
	</div>
</template>

<style scoped>
.drawer {
	flex-shrink: 0;
	background: var(--bg2);
	border-top: 1px solid var(--a09);
}
.head {
	display: flex;
	align-items: center;
	gap: 12px;
	height: 40px;
	padding: 0 16px;
	font-size: 11.5px;
}
.title {
	font-size: 14px;
	font-weight: 700;
	color: var(--t0);
}
.counts {
	color: var(--t4);
}
.c-run {
	color: var(--blue);
}
.c-done {
	color: var(--green);
}
.c-fail {
	color: var(--red);
}
.spacer {
	flex: 1;
}
.btn {
	border-radius: 7px;
	padding: 5px 12px;
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
</style>

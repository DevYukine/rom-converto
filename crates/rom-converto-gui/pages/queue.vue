<script setup lang="ts">
import { useQueueStore } from "~/stores/queue";
import { useUiStore } from "~/stores/ui";
import { useJobConcurrency } from "~/composables/useJobConcurrency";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";

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
				<PrimaryButton v-if="showStartPause" @click="queue.queueActive ? queue.pause() : queue.start()">
					{{ queue.queueActive ? "Pause" : "Start" }}
				</PrimaryButton>
				<PrimaryButton variant="outlined" :disabled="!queue.failed.length" @click="queue.retryFailed()">Retry failed</PrimaryButton>
				<PrimaryButton variant="outlined" :disabled="!queue.finished.length" @click="queue.clearFinished()">Clear finished</PrimaryButton>
				<PrimaryButton
					:variant="queue.running.length || queue.queued.length ? 'destructive' : 'outlined'"
					:disabled="!queue.running.length && !queue.queued.length"
					@click="queue.cancelAll()"
				>
					Cancel all
				</PrimaryButton>
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
</style>

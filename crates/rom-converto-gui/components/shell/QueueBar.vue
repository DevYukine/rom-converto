<script setup lang="ts">
import { useQueueStore } from "~/stores/queue";

const queue = useQueueStore();
const countsId = useId();
</script>

<template>
	<div
		class="qbar"
		tabindex="0"
		role="button"
		aria-label="Global queue"
		:aria-describedby="countsId"
		:aria-expanded="queue.drawerOpen"
		@click="queue.drawerOpen = !queue.drawerOpen"
		@keydown.enter="queue.drawerOpen = !queue.drawerOpen"
		@keydown.space.prevent="queue.drawerOpen = !queue.drawerOpen"
	>
		<span class="chev">
			<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<path :d="queue.drawerOpen ? 'M6 9l6 6 6-6' : 'M6 15l6-6 6 6'" />
			</svg>
		</span>
		<span class="label">Global queue</span>
		<span :id="countsId" class="counts">
			<b class="c-run" :class="{ zero: !queue.counts.running }">{{ queue.counts.running }} running</b> ·
			<span :class="{ zero: !queue.counts.queued }">{{ queue.counts.queued }} queued</span> ·
			<b class="c-done" :class="{ zero: !queue.counts.done }">{{ queue.counts.done }} done</b> ·
			<b class="c-fail" :class="{ zero: !queue.counts.failed }">{{ queue.counts.failed }} failed</b>
		</span>
		<span class="bar"><span class="fill" :style="{ width: queue.avgRunningPct + '%' }" /></span>
		<span class="speed">{{ queue.statusText }}</span>
		<span class="saved">{{ queue.savedGiB }} GiB saved</span>
	</div>
</template>

<style scoped>
.qbar {
	display: flex;
	align-items: center;
	gap: 16px;
	height: var(--queuebar-h);
	flex: none;
	padding: 0 18px;
	background: var(--bg2);
	border-top: 1px solid var(--a10);
	cursor: pointer;
	font-size: var(--fs-sm);
}
.qbar:hover {
	background: var(--bg2h);
}
.chev {
	display: flex;
	flex: none;
	align-items: center;
	justify-content: center;
	width: var(--ctl-h);
	height: var(--ctl-h);
	border: 1px solid var(--a20);
	border-radius: var(--r-sm);
	background: var(--a07);
	color: var(--t3);
}
.label {
	font-size: var(--fs-sm);
	font-weight: 700;
	color: var(--t0);
}
.counts {
	color: var(--t4);
	white-space: nowrap;
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
.counts .zero {
	color: var(--t5);
}
.bar {
	flex: 1;
	min-width: 24px;
	height: 6px;
	border-radius: var(--r-sm);
	background: var(--a10);
	overflow: hidden;
}
.fill {
	display: block;
	height: 100%;
	background: var(--fill);
	transition: width 0.4s;
}
.speed {
	font-family: var(--font-mono);
	color: var(--t3);
	white-space: nowrap;
}
.saved {
	font-family: var(--font-mono);
	color: var(--green);
	white-space: nowrap;
}
@media (max-width: 899px) {
	.label {
		display: none;
	}
	.qbar {
		gap: 12px;
	}
}
</style>

<script setup lang="ts">
import { useQueueStore } from "~/stores/queue";

const queue = useQueueStore();
</script>

<template>
	<div
		class="qbar"
		tabindex="0"
		role="button"
		aria-label="Toggle queue drawer"
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
		<span class="counts" aria-live="polite">
			<b class="c-run">{{ queue.counts.running }} running</b> ·
			{{ queue.counts.queued }} queued ·
			<b class="c-done">{{ queue.counts.done }} done</b> ·
			<b class="c-fail">{{ queue.counts.failed }} failed</b>
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
	height: 46px;
	padding: 0 18px;
	background: var(--bg2);
	border-top: 1px solid var(--a10);
	cursor: pointer;
	font-size: 11.5px;
}
.qbar:hover {
	background: var(--bg2h);
}
.chev {
	display: flex;
	align-items: center;
	justify-content: center;
	width: 26px;
	height: 26px;
	border: 1px solid var(--a20);
	border-radius: 7px;
	background: var(--a07);
	color: var(--t3);
}
.label {
	font-size: 12px;
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
.bar {
	flex: 1;
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
.speed {
	font-family: ui-monospace, monospace;
	color: var(--t3);
}
.saved {
	font-family: ui-monospace, monospace;
	color: var(--green);
}
</style>

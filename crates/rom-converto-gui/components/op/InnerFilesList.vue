<script setup lang="ts">
import type { InnerFile } from "~/lib/inspect-view";

defineProps<{ title: string; items: InnerFile[] }>();
</script>

<template>
	<h4>{{ title }}</h4>
	<div v-if="items.length === 0" class="rc-inner-files__empty">No inner file listing for this format.</div>
	<div v-for="(item, i) in items" :key="`${item.name}-${i}`" class="rc-inner-files__row">
		<span class="rc-inner-files__name">{{ item.name }}</span>
		<span class="rc-inner-files__detail">{{ item.detail }}</span>
	</div>
</template>

<style scoped>
h4 {
	margin: 0 0 6px;
	font-size: 10.5px;
	font-weight: 700;
	text-transform: uppercase;
	letter-spacing: 0.8px;
	color: var(--t4);
}

.rc-inner-files__empty {
	font-size: 11.5px;
	color: var(--t5);
	margin: 4px 0 0;
}

.rc-inner-files__row {
	display: grid;
	grid-template-columns: minmax(0, 1fr) minmax(0, auto);
	gap: 10px;
	align-items: start;
	padding: 3px 0;
}

.rc-inner-files__name {
	font-family: ui-monospace, monospace;
	font-size: 11.5px;
	color: var(--t3);
	overflow-wrap: anywhere;
	white-space: normal;
}

.rc-inner-files__detail {
	font-family: ui-monospace, monospace;
	font-size: 11px;
	color: var(--t4);
	overflow-wrap: anywhere;
	white-space: normal;
	text-align: right;
}
</style>

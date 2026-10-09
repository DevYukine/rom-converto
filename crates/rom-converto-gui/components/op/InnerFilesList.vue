<script setup lang="ts">
import type { InnerFile } from "~/lib/inspect-view";

defineProps<{ title: string; items: InnerFile[] }>();
</script>

<template>
	<div class="rc-inner-files">
		<h4 class="rc-inner-files__title">{{ title }}</h4>
		<div v-if="items.length === 0" class="rc-inner-files__empty">No inner file listing for this format.</div>
		<div v-for="(item, i) in items" :key="`${item.name}-${i}`" class="rc-inner-files__row">
			<span class="rc-inner-files__name" :title="item.name">{{ item.name }}</span>
			<span class="rc-inner-files__detail" :title="item.detail">{{ item.detail }}</span>
		</div>
	</div>
</template>

<style scoped>
.rc-inner-files__title {
	margin: 0 0 8px;
	font-size: var(--fs-lg);
	font-weight: 600;
	color: var(--t1);
	white-space: nowrap;
}

.rc-inner-files__empty {
	font-size: var(--fs-md);
	color: var(--t5);
	line-height: var(--lh-body);
	margin: 4px 0 0;
}

.rc-inner-files__row {
	display: grid;
	grid-template-columns: minmax(0, 1fr) minmax(0, auto);
	gap: 16px;
	align-items: center;
	min-height: 32px;
	padding: 6px 0;
}

.rc-inner-files__row + .rc-inner-files__row {
	border-top: 1px solid var(--a06);
}

.rc-inner-files__name {
	font-family: var(--font-mono);
	font-size: var(--fs-sm);
	color: var(--t3);
	overflow-wrap: anywhere;
}

.rc-inner-files__detail {
	font-family: var(--font-mono);
	font-size: var(--fs-xs);
	color: var(--t4);
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
	text-align: right;
}
</style>

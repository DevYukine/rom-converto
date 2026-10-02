<script setup lang="ts">
import { formatBytes } from "~/lib/inspect/shared";
import type { StagedItem } from "~/lib/opdefs/types";

defineProps<{
	items: StagedItem[];
	label: string;
}>();

const emit = defineEmits<{ remove: [id: string]; clear: [] }>();

function meta(item: StagedItem): string {
	const parts = [!item.dir && item.size > 0 ? formatBytes(item.size) : ""];
	if (item.outExt) parts.push(`→ .${item.outExt}`);
	return parts.filter(Boolean).join(" · ");
}
</script>

<template>
	<div class="rc-staged">
		<div class="rc-staged__head">
			<span class="rc-staged__title">{{ label }}</span>
			<button type="button" class="rc-staged__clear" @click="emit('clear')">Clear all</button>
		</div>
		<div class="rc-staged__items">
			<div v-for="item in items" :key="item.id" class="rc-staged__row">
				<span class="rc-staged__name" :title="item.path">{{ item.name }}</span>
				<span v-if="meta(item)" class="rc-staged__meta">{{ meta(item) }}</span>
				<button type="button" class="rc-staged__remove" title="Remove" :aria-label="`Remove ${item.name}`" @click="emit('remove', item.id)">✕</button>
			</div>
		</div>
	</div>
</template>

<style scoped>
.rc-staged {
	border: 1px solid var(--a10);
	border-radius: var(--r-lg);
	background: var(--card);
	min-width: 0;
}

.rc-staged__head {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 16px;
	padding: 10px 14px;
	border-bottom: 1px solid var(--a06);
}

.rc-staged__title {
	font-size: var(--fs-md);
	font-weight: 600;
	color: var(--t1);
	white-space: nowrap;
}

.rc-staged__clear {
	background: none;
	border: none;
	color: var(--blue);
	font-size: var(--fs-sm);
	cursor: pointer;
	padding: 0;
	white-space: nowrap;
	flex: none;
}

.rc-staged__items {
	max-height: 280px;
	overflow-y: auto;
}

.rc-staged__row {
	display: grid;
	grid-template-columns: minmax(0, 1fr) auto auto;
	align-items: center;
	gap: 10px;
	padding: 7px 14px;
}

.rc-staged__row + .rc-staged__row {
	border-top: 1px solid var(--a06);
}

.rc-staged__name {
	color: var(--t0);
	font-size: var(--fs-md);
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
	min-width: 0;
}

.rc-staged__meta {
	font-family: var(--font-mono);
	font-size: var(--fs-xs);
	color: var(--t5);
	white-space: nowrap;
	grid-column: 2;
}

.rc-staged__remove {
	background: none;
	border: none;
	color: var(--t5);
	cursor: pointer;
	font-size: var(--fs-xs);
	padding: 0;
	width: var(--ctl-h);
	height: var(--ctl-h);
	border-radius: var(--r-sm);
	grid-column: 3;
}

.rc-staged__remove:hover {
	color: var(--red);
}
</style>

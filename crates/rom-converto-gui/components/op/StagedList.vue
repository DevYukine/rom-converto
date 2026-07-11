<script setup lang="ts">
import type { StagedItem } from "~/lib/opdefs/types";

defineProps<{
	items: StagedItem[];
	label: string;
	consoleName: string;
}>();

const emit = defineEmits<{ remove: [id: string]; clear: [] }>();

const UNITS = ["B", "KiB", "MiB", "GiB", "TiB"];

function humanSize(bytes: number): string {
	if (bytes <= 0) return "…";
	let n = bytes;
	let u = 0;
	while (n >= 1024 && u < UNITS.length - 1) {
		n /= 1024;
		u++;
	}
	return `${n.toFixed(u === 0 ? 0 : 1)} ${UNITS[u]}`;
}

function meta(item: StagedItem, consoleName: string): string {
	const parts = [consoleName, humanSize(item.size)];
	if (item.outExt) parts.push(`→ .${item.outExt}`);
	return parts.join(" · ");
}
</script>

<template>
	<div class="rc-staged">
		<div class="rc-staged__head">
			<span class="rc-staged__title">{{ label }}</span>
			<button type="button" class="rc-staged__clear" @click="emit('clear')">Clear all</button>
		</div>
		<div v-for="item in items" :key="item.id" class="rc-staged__row">
			<span class="rc-staged__dot" />
			<span class="rc-staged__name">{{ item.name }}</span>
			<span class="rc-staged__meta">{{ meta(item, consoleName) }}</span>
			<button type="button" class="rc-staged__remove" title="Remove" @click="emit('remove', item.id)">✕</button>
		</div>
	</div>
</template>

<style scoped>
.rc-staged {
	border: 1px solid var(--a10);
	border-radius: 10px;
	background: var(--card);
}

.rc-staged__head {
	display: flex;
	align-items: center;
	justify-content: space-between;
	padding: 10px 14px;
	border-bottom: 1px solid var(--a06);
}

.rc-staged__title {
	font-size: 10.5px;
	font-weight: 700;
	text-transform: uppercase;
	letter-spacing: 0.8px;
	color: var(--t4);
}

.rc-staged__clear {
	background: none;
	border: none;
	color: var(--blue);
	font-size: 11px;
	cursor: pointer;
	padding: 0;
}

.rc-staged__row {
	display: flex;
	align-items: center;
	gap: 10px;
	padding: 7px 14px;
}

.rc-staged__dot {
	width: 7px;
	height: 7px;
	border-radius: 50%;
	background: var(--t5);
	flex-shrink: 0;
}

.rc-staged__name {
	color: var(--t0);
	font-size: 12px;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
	min-width: 0;
}

.rc-staged__meta {
	margin-left: auto;
	font-family: ui-monospace, monospace;
	font-size: 10px;
	color: var(--t5);
	white-space: nowrap;
}

.rc-staged__remove {
	background: none;
	border: none;
	color: var(--t5);
	cursor: pointer;
	font-size: 11px;
	padding: 0;
}

.rc-staged__remove:hover {
	color: var(--red);
}
</style>

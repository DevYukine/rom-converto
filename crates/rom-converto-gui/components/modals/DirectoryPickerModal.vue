<script setup lang="ts">
import { computed } from "vue";
import ModalShell from "~/components/modals/ModalShell.vue";
import { open } from "~/lib/ipc";
import { useUiStore } from "~/stores/ui";

const props = defineProps<{
	modelValue: string;
	clearLabel?: string;
}>();

const ui = useUiStore();

const emit = defineEmits<{
	"update:modelValue": [value: string];
	close: [];
}>();

const rows = computed(() => [
	...(props.clearLabel ? [{ label: props.clearLabel, value: "" }] : []),
	...(props.modelValue && !ui.recentOutputDirs.includes(props.modelValue)
		? [{ label: props.modelValue, value: props.modelValue }]
		: []),
	...ui.recentOutputDirs.map((dir) => ({ label: dir, value: dir })),
]);

function select(value: string) {
	ui.rememberOutputDir(value);
	emit("update:modelValue", value);
	emit("close");
}

async function chooseFolder() {
	const picked = await open({ directory: true, multiple: false });
	if (typeof picked === "string" && picked) select(picked);
}
</script>

<template>
	<ModalShell title="Output directory" :width="460" @close="emit('close')">
		<div class="rc-rows">
			<button
				v-for="row in rows"
				:key="row.label"
				type="button"
				class="rc-row"
				:title="row.label"
				:aria-pressed="row.value === modelValue"
				@click="select(row.value)"
			>
				<span class="rc-path" :class="{ 'rc-path--mono': row.value }">{{ row.label }}</span>
				<span class="rc-check" aria-hidden="true">{{ row.value === modelValue ? "✓" : "" }}</span>
			</button>
			<button type="button" class="rc-row" title="Choose another folder…" @click="chooseFolder">
				<span class="rc-path">Choose another folder…</span>
				<svg class="rc-folder" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true">
					<path d="M3 7h6l2 2h10v10H3z" />
				</svg>
			</button>
		</div>
	</ModalShell>
</template>

<style scoped>
.rc-rows {
	display: flex;
	flex-direction: column;
	gap: 4px;
}

.rc-row {
	width: 100%;
	display: grid;
	grid-template-columns: minmax(0, 1fr) 16px;
	align-items: center;
	gap: 12px;
	min-height: 36px;
	padding: 6px 10px;
	border: none;
	border-radius: var(--r-sm);
	background: transparent;
	color: var(--t2);
	font-size: var(--fs-md);
	cursor: pointer;
	white-space: nowrap;
	text-align: left;
}

.rc-row:hover {
	background: var(--a08);
}

.rc-path {
	min-width: 0;
	overflow: hidden;
	text-overflow: ellipsis;
}

.rc-folder {
	color: var(--t5);
}

.rc-path--mono {
	font-family: var(--font-mono);
}

.rc-check {
	text-align: center;
	color: var(--green);
	font-size: var(--fs-sm);
	font-weight: 600;
}
</style>

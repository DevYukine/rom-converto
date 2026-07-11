<script setup lang="ts">
import { computed } from "vue";
import ModalShell from "~/components/modals/ModalShell.vue";

export interface DryRunLine {
	source: string;
	output: string;
	note: string;
	conflict?: boolean;
}

const props = defineProps<{
	command: string;
	lines: DryRunLine[];
}>();

const emit = defineEmits<{ close: [] }>();

const fullCommand = computed(() => `${props.command} --dry-run`);

const { show: showToast } = useToast();

function copy() {
	navigator.clipboard?.writeText(fullCommand.value);
	showToast("Copied");
}
</script>

<template>
	<ModalShell title="Dry run" :width="680" @close="emit('close')">
		<template #header-extra>
			<button type="button" class="rc-cli" title="Click to copy" @click="copy">
				$ {{ fullCommand }}
			</button>
		</template>

		<div class="rc-rows">
			<div v-for="(line, i) in lines" :key="i" class="rc-row">
				<div class="rc-source">{{ line.source }}</div>
				<div class="rc-output">→ {{ line.output }}</div>
				<div class="rc-note" :class="{ conflict: line.conflict }">{{ line.note }}</div>
			</div>
		</div>

		<template #footer>
			<span class="rc-hint">Nothing was written. Conflicts show the resolution the current policy would apply.</span>
			<div class="rc-spacer" />
			<button type="button" class="rc-outlined" @click="emit('close')">Close</button>
		</template>
	</ModalShell>
</template>

<style scoped>
.rc-cli {
	flex: 1;
	min-width: 0;
	background: var(--bg2);
	border: 1px solid var(--a14);
	border-radius: 6px;
	padding: 5px 10px;
	color: var(--t4);
	font-family: ui-monospace, monospace;
	font-size: 10.5px;
	cursor: pointer;
	text-align: left;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-cli:hover {
	border-color: var(--a30);
	color: var(--t3);
}

.rc-rows {
	display: flex;
	flex-direction: column;
	gap: 8px;
}

.rc-row {
	border: 1px solid var(--a10);
	border-radius: 8px;
	padding: 8px 10px;
	display: flex;
	flex-direction: column;
	gap: 2px;
}

.rc-source {
	color: var(--t0);
	font-size: 12px;
}

.rc-output {
	font-family: ui-monospace, monospace;
	font-size: 11px;
	color: var(--t4);
}

.rc-note {
	font-size: 11px;
	color: var(--green);
}

.rc-note.conflict {
	color: var(--yellow);
}

.rc-hint {
	font-size: 11.5px;
	color: var(--t4);
}

.rc-spacer {
	flex: 1;
}

.rc-outlined {
	background: none;
	border: 1px solid var(--a18);
	color: var(--t3);
	border-radius: 8px;
	padding: 6px 16px;
	font-size: 12.5px;
	cursor: pointer;
}

.rc-outlined:hover {
	border-color: var(--a40);
}
</style>

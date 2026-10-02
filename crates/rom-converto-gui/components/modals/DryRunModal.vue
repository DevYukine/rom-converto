<script setup lang="ts">
import { computed } from "vue";
import ModalShell from "~/components/modals/ModalShell.vue";
import CliChip from "~/components/ui/CliChip.vue";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";

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

function copied() {
	showToast("Copied");
}
</script>

<template>
	<ModalShell title="Dry run" :width="900" @close="emit('close')">
		<template #header-extra>
			<CliChip :command="fullCommand" @copy="copied" />
		</template>

		<div class="rc-rows">
			<div v-for="(line, i) in lines" :key="i" class="rc-row" :class="{ 'rc-row--source-only': !line.output }">
				<div class="rc-source">{{ line.source }}</div>
				<span v-if="line.output" class="rc-arrow" aria-hidden="true">→</span>
				<div v-if="line.output" class="rc-output">{{ line.output }}</div>
				<div class="rc-note" :class="{ conflict: line.conflict }">{{ line.note }}</div>
			</div>
		</div>

		<template #footer>
			<span class="rc-hint">Nothing was written. Conflicts show the resolution the current policy would apply.</span>
			<div class="rc-footer-actions">
				<PrimaryButton variant="outlined" @click="emit('close')">Close</PrimaryButton>
			</div>
		</template>
	</ModalShell>
</template>

<style scoped>
.rc-rows {
	display: flex;
	flex-direction: column;
	gap: 8px;
}

.rc-row {
	border: 1px solid var(--a10);
	border-radius: var(--r-md);
	padding: 8px 10px;
	display: grid;
	grid-template-columns: minmax(0, 1fr) 16px minmax(0, 1fr);
	gap: 4px 12px;
}

.rc-row--source-only {
	grid-template-columns: minmax(0, 1fr);
}

.rc-source {
	color: var(--t0);
	font-size: var(--fs-md);
	font-family: var(--font-mono);
	overflow-wrap: anywhere;
}

.rc-output {
	font-family: var(--font-mono);
	font-size: var(--fs-sm);
	overflow-wrap: anywhere;
	color: var(--t4);
}

.rc-arrow {
	color: var(--t5);
}

.rc-note {
	font-size: var(--fs-sm);
	grid-column: 1 / -1;
	line-height: var(--lh-body);
	text-wrap: pretty;
	color: var(--green);
}

.rc-note.conflict {
	color: var(--yellow);
}

.rc-hint {
	flex: 1 1 260px;
	min-width: 0;
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	color: var(--t5);
	text-wrap: pretty;
}

.rc-footer-actions {
	margin-left: auto;
}
</style>

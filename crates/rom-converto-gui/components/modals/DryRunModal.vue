<script setup lang="ts">
import { computed } from "vue";
import ModalShell from "~/components/modals/ModalShell.vue";
import CliChip from "~/components/ui/CliChip.vue";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";
import VirtualList from "~/components/ui/VirtualList.vue";
import type { DryRunLine } from "~/lib/dry-run";

// Fixed row height: one line for the paths, one for the note, both truncated
// with the full text on the cells' titles.
const ROW_HEIGHT = 50;

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

		<div class="rc-rows" tabindex="0" role="region" aria-label="Planned changes">
			<VirtualList :items="lines" :row-height="ROW_HEIGHT" :key-of="(line, index) => `${index}:${line.source}`">
				<template #default="{ item: line }">
					<div class="rc-row" :class="{ 'rc-row--source-only': !line.output }">
						<div class="rc-source rc-picker__value--path" :title="line.source"><bdi dir="ltr">{{ line.source }}</bdi></div>
						<span v-if="line.output" class="rc-arrow" aria-hidden="true">→</span>
						<div v-if="line.output" class="rc-output rc-picker__value--path" :title="line.output"><bdi dir="ltr">{{ line.output }}</bdi></div>
						<div class="rc-note" :class="{ conflict: line.conflict, muted: line.muted }" :title="line.note">{{ line.note }}</div>
					</div>
				</template>
			</VirtualList>
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
	/* Focusable so keyboard users can reach the plan and scroll it (the rows
	   hold no controls); the scrolling stays on the modal body, which has the
	   definite height VirtualList's windowing needs. */
	min-width: 0;
}

.rc-row {
	/* Fills the fixed-height wrapper VirtualList sizes from ROW_HEIGHT. */
	height: 100%;
	border-bottom: 1px solid var(--a10);
	padding: 5px 2px;
	display: grid;
	grid-template-columns: minmax(0, 1fr) 16px minmax(0, 1fr);
	gap: 0 12px;
	align-content: center;
}

.rc-row--source-only {
	grid-template-columns: minmax(0, 1fr);
}

.rc-row > * {
	min-width: 0;
	overflow: hidden;
	white-space: nowrap;
	text-overflow: ellipsis;
}

.rc-source {
	color: var(--t0);
	font-size: var(--fs-md);
	font-family: var(--font-mono);
}

.rc-output {
	font-family: var(--font-mono);
	font-size: var(--fs-sm);
	color: var(--t4);
}

.rc-arrow {
	color: var(--t5);
	text-align: center;
}

.rc-note {
	grid-column: 1 / -1;
	font-size: var(--fs-sm);
	color: var(--green);
}

.rc-note.conflict {
	color: var(--yellow);
}

.rc-note.muted {
	color: var(--t5);
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

<script setup lang="ts">
import ModalShell from "~/components/modals/ModalShell.vue";

defineProps<{
	title: string;
	lines: string[];
}>();

const emit = defineEmits<{ close: [] }>();
</script>

<template>
	<ModalShell :title="title" :width="560" @close="emit('close')">
		<div class="rc-lines">
			<p v-for="(line, i) in lines" :key="i" class="rc-line">{{ line }}</p>
		</div>

		<template #footer>
			<div class="rc-spacer" />
			<button type="button" class="rc-outlined" @click="emit('close')">Close</button>
		</template>
	</ModalShell>
</template>

<style scoped>
.rc-lines {
	display: flex;
	flex-direction: column;
	gap: 8px;
}

.rc-line {
	margin: 0;
	font-family: ui-monospace, monospace;
	font-size: 12px;
	line-height: 1.5;
	color: var(--t3);
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

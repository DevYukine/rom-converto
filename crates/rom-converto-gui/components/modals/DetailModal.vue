<script setup lang="ts">
import ModalShell from "~/components/modals/ModalShell.vue";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";

withDefaults(
	defineProps<{
		title: string;
		lines: string[];
		tone?: "error" | "plain";
	}>(),
	{ tone: "plain" },
);

const emit = defineEmits<{ close: [] }>();
</script>

<template>
	<ModalShell :title="title" :width="480" @close="emit('close')">
		<div class="rc-lines">
			<p v-for="(line, i) in lines" :key="i" class="rc-line" :class="{ 'rc-line--error': tone === 'error' }">{{ line }}</p>
		</div>

		<template #footer>
			<div class="rc-footer-actions">
				<PrimaryButton variant="outlined" @click="emit('close')">Close</PrimaryButton>
			</div>
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
	font-family: var(--font-mono);
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	overflow-wrap: anywhere;
	color: var(--t3);
}

.rc-line--error {
	color: var(--red);
}

.rc-footer-actions {
	margin-left: auto;
}
</style>

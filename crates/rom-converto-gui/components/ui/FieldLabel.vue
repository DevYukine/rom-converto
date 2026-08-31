<script setup lang="ts">
defineOptions({ inheritAttrs: false });
defineProps<{
	label: string;
	tooltip?: string;
}>();
</script>

<template>
	<span class="rc-field-label">
		<!-- attrs (label ids for aria-labelledby) go on the text span so the tooltip stays out of the accessible name -->
		<span v-bind="$attrs">{{ label }}</span>
		<InfoTooltip v-if="tooltip" :message="tooltip" placement="top" class="rc-field-label__tip">
			<span class="rc-field-label__icon" aria-hidden="true">i</span>
		</InfoTooltip>
	</span>
</template>

<style scoped>
.rc-field-label {
	font-size: 12px;
	color: var(--t2);
	display: inline-flex;
	align-items: center;
	gap: 4px;
}

.rc-field-label__tip {
	line-height: 0;
}

.rc-field-label__icon {
	display: inline-flex;
	align-items: center;
	justify-content: center;
	width: 12px;
	height: 12px;
	border-radius: 50%;
	border: 1px solid var(--t5);
	color: var(--t5);
	font-size: 9px;
	font-style: italic;
	line-height: 1;
	cursor: help;
}
</style>

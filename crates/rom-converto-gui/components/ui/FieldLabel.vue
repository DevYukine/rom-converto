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
		<span v-bind="$attrs">{{ label }}</span><template v-if="tooltip">&nbsp;<InfoTooltip :message="tooltip" label="More info" class="rc-field-label__tip">
			<svg class="rc-field-label__icon" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true">
				<circle cx="12" cy="12" r="9" />
				<path d="M12 11v5m0-9v.5" />
			</svg>
		</InfoTooltip></template>
	</span>
</template>

<style scoped>
.rc-field-label {
	font-size: var(--fs-md);
	color: var(--t2);
	display: inline;
	line-height: var(--lh-body);
	white-space: normal;
}

.rc-field-label__tip {
	line-height: 0;
	vertical-align: -2px;
}

.rc-field-label__icon {
	flex: none;
	color: var(--t5);
	cursor: help;
}

.rc-field-label__tip:hover .rc-field-label__icon,
.rc-field-label__tip:focus-visible .rc-field-label__icon {
	color: var(--t3);
}
</style>

<script setup lang="ts">
defineOptions({ inheritAttrs: false });
const props = withDefaults(
	defineProps<{
		modelValue: string;
		options: { label: string; value: string }[];
		label?: string;
		tooltip?: string;
		// Label left, control right; stacks to a full-width block when the
		// enclosing `field` container is narrower than 520px. Without it the
		// control sits inline at its natural width.
		row?: boolean;
		disabled?: boolean;
	}>(),
	{ row: false },
);

const emit = defineEmits<{
	"update:modelValue": [value: string];
}>();

const labelId = useId();

function select(value: string) {
	if (!props.disabled && value !== props.modelValue) emit("update:modelValue", value);
}
</script>

<template>
	<div class="rc-segmented-wrap" :class="{ 'rc-segmented-wrap--row': row }">
		<FieldLabel v-if="label" :id="labelId" :label="label" :tooltip="tooltip" />
		<div
			role="group"
			:aria-label="$attrs['aria-label'] as string | undefined"
			:aria-labelledby="label ? labelId : ($attrs['aria-labelledby'] as string | undefined)"
			class="rc-segmented"
		>
			<button
				v-for="option in options"
				:key="option.value"
				type="button"
				:disabled="disabled"
				:aria-pressed="option.value === modelValue"
				class="rc-segmented__option"
				:class="{ 'rc-segmented__option--active': option.value === modelValue }"
				:title="option.label"
				@click="select(option.value)"
			>
				{{ option.label }}
			</button>
		</div>
	</div>
</template>

<style scoped>
.rc-segmented-wrap {
	display: inline-flex;
	flex-direction: column;
	align-items: flex-start;
	gap: 6px;
	min-width: 0;
}

.rc-segmented-wrap--row {
	display: flex;
	flex-direction: row;
	flex-wrap: wrap;
	align-items: center;
	justify-content: space-between;
	gap: 6px 16px;
	width: 100%;
}

.rc-segmented-wrap--row > .rc-segmented {
	margin-left: auto;
}

.rc-segmented-wrap--row .rc-segmented__option {
	min-width: 72px;
}

@container field (width < 520px) {
	.rc-segmented-wrap--row {
		flex-direction: column;
		align-items: stretch;
	}

	.rc-segmented-wrap--row > .rc-segmented {
		margin-left: 0;
	}

	.rc-segmented-wrap--row > .rc-segmented > .rc-segmented__option {
		flex: 1 1 0;
		min-width: 0;
	}
}

.rc-segmented {
	display: inline-flex;
	min-width: 0;
	max-width: 100%;
	min-height: var(--ctl-h);
	background: var(--bg2);
	border: 1px solid var(--a14);
	border-radius: calc(var(--r-sm) + 1px);
	padding: 2px;
}

.rc-segmented__option {
	min-width: 0;
	height: 24px;
	padding: 0 12px;
	border: none;
	border-radius: calc(var(--r-sm) - 1px);
	background: transparent;
	color: var(--t4);
	font-size: var(--fs-sm);
	font-weight: 400;
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
	cursor: pointer;
}

.rc-segmented__option:disabled {
	cursor: not-allowed;
	opacity: 0.5;
}

.rc-segmented__option--active {
	background: var(--a14);
	color: var(--t0);
	font-weight: 600;
}

:global([data-theme="light"] .rc-segmented__option--active) {
	background: var(--card);
	box-shadow: 0 1px 2px var(--a20);
}
</style>

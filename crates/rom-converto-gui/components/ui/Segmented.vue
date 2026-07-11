<script setup lang="ts">
const props = withDefaults(
	defineProps<{
		modelValue: string;
		options: { label: string; value: string }[];
		label?: string;
		size?: "sm" | "md";
		disabled?: boolean;
		disabledOptions?: string[];
		disabledReason?: string;
	}>(),
	{ size: "sm" },
);

const emit = defineEmits<{
	"update:modelValue": [value: string];
}>();

const labelId = useId();

function isDisabled(value: string) {
	return props.disabled || (props.disabledOptions?.includes(value) ?? false);
}

function select(value: string) {
	if (!isDisabled(value) && value !== props.modelValue) emit("update:modelValue", value);
}
</script>

<template>
	<div class="rc-segmented-wrap">
		<span v-if="label" :id="labelId" class="rc-segmented-label">{{ label }}</span>
		<div role="group" :aria-labelledby="label ? labelId : undefined" class="rc-segmented" :class="`rc-segmented--${size}`">
			<template v-for="option in options" :key="option.value">
				<InfoTooltip v-if="isDisabled(option.value) && disabledReason" :message="disabledReason">
					<button type="button" disabled class="rc-segmented__option" :aria-pressed="option.value === modelValue">
						{{ option.label }}
					</button>
				</InfoTooltip>
				<button
					v-else
					type="button"
					:disabled="isDisabled(option.value)"
					:aria-pressed="option.value === modelValue"
					class="rc-segmented__option"
					:class="{ 'rc-segmented__option--active': option.value === modelValue }"
					@click="select(option.value)"
				>
					{{ option.label }}
				</button>
			</template>
		</div>
	</div>
</template>

<style scoped>
.rc-segmented-wrap {
	display: flex;
	flex-direction: column;
	gap: 6px;
}

.rc-segmented-label {
	font-size: 12px;
	color: var(--t2);
}

.rc-segmented {
	display: inline-flex;
	border: 1px solid var(--a14);
	border-radius: 7px;
	overflow: hidden;
}

.rc-segmented__option {
	border: none;
	background: transparent;
	color: var(--t4);
	font-size: 12px;
	font-weight: 400;
	cursor: pointer;
}

.rc-segmented--sm .rc-segmented__option {
	padding: 4px 14px;
}

.rc-segmented--md .rc-segmented__option {
	padding: 5px 14px;
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
</style>

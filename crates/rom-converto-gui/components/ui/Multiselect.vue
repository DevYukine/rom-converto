<script setup lang="ts">
const props = defineProps<{
	modelValue: string[];
	options: { value: string; label: string }[];
	label?: string;
	max?: number;
	placeholder?: string;
}>();

const emit = defineEmits<{
	"update:modelValue": [value: string[]];
}>();

const labelId = useId();

function orderOf(value: string): number {
	return props.modelValue.indexOf(value);
}

function isSelected(value: string): boolean {
	return orderOf(value) !== -1;
}

function isDisabled(value: string): boolean {
	return !isSelected(value) && !!props.max && props.modelValue.length >= props.max;
}

function toggle(value: string) {
	if (isSelected(value)) {
		emit(
			"update:modelValue",
			props.modelValue.filter((v) => v !== value),
		);
	} else if (!isDisabled(value)) {
		emit("update:modelValue", [...props.modelValue, value]);
	}
}
</script>

<template>
	<div class="rc-multiselect-wrap">
		<span v-if="label" :id="labelId" class="rc-multiselect-label">{{ label }}</span>
		<div role="group" :aria-labelledby="label ? labelId : undefined" class="rc-multiselect">
			<button
				v-for="option in options"
				:key="option.value"
				type="button"
				:disabled="isDisabled(option.value)"
				:aria-pressed="isSelected(option.value)"
				class="rc-multiselect__chip"
				:class="{ 'rc-multiselect__chip--active': isSelected(option.value) }"
				@click="toggle(option.value)"
			>
				<span v-if="max && isSelected(option.value)" class="rc-multiselect__badge">{{ orderOf(option.value) + 1 }}</span>
				{{ option.label }}
			</button>
		</div>
		<p v-if="placeholder && modelValue.length === 0" class="rc-multiselect__placeholder">{{ placeholder }}</p>
	</div>
</template>

<style scoped>
.rc-multiselect-wrap {
	display: flex;
	flex-direction: column;
	gap: 6px;
}

.rc-multiselect-label {
	font-size: 12px;
	color: var(--t2);
}

.rc-multiselect {
	display: flex;
	flex-wrap: wrap;
	gap: 6px;
}

.rc-multiselect__chip {
	display: inline-flex;
	align-items: center;
	gap: 5px;
	border: 1px solid var(--a14);
	border-radius: 999px;
	background: transparent;
	color: var(--t4);
	font-size: 12px;
	font-weight: 400;
	padding: 4px 12px;
	cursor: pointer;
}

.rc-multiselect__chip:disabled {
	cursor: not-allowed;
	opacity: 0.5;
}

.rc-multiselect__chip--active {
	background: var(--a14);
	color: var(--t0);
	font-weight: 600;
}

.rc-multiselect__badge {
	display: inline-flex;
	align-items: center;
	justify-content: center;
	width: 14px;
	height: 14px;
	border-radius: 50%;
	background: var(--blue);
	color: var(--bg2);
	font-size: 9px;
	font-weight: 700;
	line-height: 1;
}

.rc-multiselect__placeholder {
	margin: 0;
	font-size: 10.5px;
	color: var(--t5);
}
</style>

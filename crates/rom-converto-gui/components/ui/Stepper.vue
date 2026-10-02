<script setup lang="ts">
const props = defineProps<{
	modelValue: number;
	min: number;
	max: number;
	label: string;
}>();
const emit = defineEmits<{ "update:modelValue": [value: number] }>();

function step(delta: number) {
	emit("update:modelValue", Math.min(props.max, Math.max(props.min, props.modelValue + delta)));
}
</script>

<template>
	<div class="rc-stepper" role="group" :aria-label="label">
		<button type="button" aria-label="Decrease" :disabled="modelValue <= min" @click="step(-1)">−</button>
		<span class="rc-stepper__value" aria-live="polite">{{ modelValue }}</span>
		<button type="button" aria-label="Increase" :disabled="modelValue >= max" @click="step(1)">+</button>
	</div>
</template>

<style scoped>
.rc-stepper {
	display: inline-flex;
	flex: none;
	align-items: center;
	height: var(--ctl-h);
	border: 1px solid var(--a16);
	border-radius: var(--r-sm);
	background: var(--field);
	white-space: nowrap;
}
.rc-stepper__value {
	min-width: var(--ctl-h);
	text-align: center;
	font-family: var(--font-mono);
	font-size: var(--fs-sm);
	color: var(--t2);
}
.rc-stepper button {
	flex: none;
	width: var(--ctl-h);
	height: 100%;
	padding: 0;
	border: none;
	background: transparent;
	color: var(--t4);
	font-size: var(--fs-md);
	line-height: 1;
	cursor: pointer;
}
.rc-stepper button:first-child {
	border-radius: var(--r-sm) 0 0 var(--r-sm);
}
.rc-stepper button:last-child {
	border-radius: 0 var(--r-sm) var(--r-sm) 0;
}
.rc-stepper button:hover:not(:disabled) {
	background: var(--field-hover);
	color: var(--t0);
}
.rc-stepper button:disabled {
	color: var(--t7);
	cursor: not-allowed;
}
</style>

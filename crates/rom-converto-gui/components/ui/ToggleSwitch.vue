<script setup lang="ts">
const props = defineProps<{
	label?: string;
	tooltip?: string;
	modelValue: boolean;
	description?: string;
	disabled?: boolean;
}>();

const emit = defineEmits<{
	"update:modelValue": [value: boolean];
}>();

const uid = useId();
const labelId = `${uid}-label`;
const descriptionId = `${uid}-description`;

function toggle() {
	if (!props.disabled) emit("update:modelValue", !props.modelValue);
}
</script>

<template>
	<div class="rc-toggle-row" :class="{ 'rc-toggle-row--disabled': disabled }">
		<div v-if="label || description" class="rc-toggle-row__text">
			<FieldLabel v-if="label" :id="labelId" :label="label" :tooltip="tooltip" />
			<p v-if="description" :id="descriptionId" class="rc-toggle-row__desc">{{ description }}</p>
		</div>
		<button
			type="button"
			role="switch"
			:aria-checked="modelValue"
			:aria-labelledby="label ? labelId : undefined"
			:aria-describedby="description ? descriptionId : undefined"
			:disabled="disabled"
			class="rc-toggle"
			:class="{ 'rc-toggle--on': modelValue }"
			@click="toggle"
		>
			<span class="rc-toggle__knob" />
		</button>
	</div>
</template>

<style scoped>
.rc-toggle-row {
	display: flex;
	align-items: center;
	flex-wrap: wrap;
	min-height: 40px;
	padding: 6px 0;
	gap: 16px;
}

.rc-toggle-row--disabled {
	opacity: 0.5;
}

.rc-toggle-row__text {
	flex: 1 1 0;
	min-width: 0;
}

.rc-toggle-row__desc {
	margin: 2px 0 0;
	font-size: var(--fs-sm);
	color: var(--t5);
	line-height: var(--lh-body);
	text-wrap: pretty;
}

.rc-toggle {
	position: relative;
	flex: none;
	width: 32px;
	height: 18px;
	border: none;
	border-radius: var(--r-lg);
	background: var(--a18);
	cursor: pointer;
	padding: 0;
}

.rc-toggle:disabled {
	cursor: not-allowed;
}

.rc-toggle--on {
	background: var(--fill);
}

.rc-toggle__knob {
	position: absolute;
	top: 2px;
	left: 2px;
	width: 14px;
	height: 14px;
	border-radius: 50%;
	background: var(--knobOff);
	transition: left 0.15s;
}

.rc-toggle--on .rc-toggle__knob {
	left: 16px;
	background: #fff;
}
</style>

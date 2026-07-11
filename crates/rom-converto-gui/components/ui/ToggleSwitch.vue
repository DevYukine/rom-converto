<script setup lang="ts">
const props = defineProps<{
	label?: string;
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
			<span v-if="label" :id="labelId" class="rc-toggle-row__label">{{ label }}</span>
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
	justify-content: space-between;
	gap: 12px;
}

.rc-toggle-row--disabled {
	opacity: 0.5;
}

.rc-toggle-row__text {
	min-width: 0;
}

.rc-toggle-row__label {
	font-size: 12px;
	color: var(--t2);
}

.rc-toggle-row__desc {
	margin: 2px 0 0;
	font-size: 10.5px;
	color: var(--t5);
	line-height: 1.45;
}

.rc-toggle {
	position: relative;
	flex-shrink: 0;
	width: 30px;
	height: 17px;
	border: none;
	border-radius: 10px;
	background: var(--a18);
	cursor: pointer;
	padding: 0;
}

.rc-toggle:disabled {
	cursor: not-allowed;
}

.rc-toggle--on {
	background: #3b82f6;
}

.rc-toggle__knob {
	position: absolute;
	top: 2px;
	left: 2px;
	width: 13px;
	height: 13px;
	border-radius: 50%;
	background: var(--knobOff);
	transition: left 0.15s;
}

.rc-toggle--on .rc-toggle__knob {
	left: 15px;
	background: #fff;
}
</style>

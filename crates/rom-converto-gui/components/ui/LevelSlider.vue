<script setup lang="ts">
const props = withDefaults(
	defineProps<{
		modelValue: number;
		min?: number;
		max?: number;
		label?: string;
		hint?: string;
		disabled?: boolean;
		formatValue?: (value: number) => string;
	}>(),
	{ min: 1, max: 22 },
);

const emit = defineEmits<{
	"update:modelValue": [value: number];
}>();

const uid = useId();
const labelId = `${uid}-label`;
const hintId = `${uid}-hint`;

const displayValue = computed(() => (props.formatValue ? props.formatValue(props.modelValue) : String(props.modelValue)));

const fillPct = computed(() => ((props.modelValue - props.min) / (props.max - props.min)) * 100);
const trackStyle = computed(() => ({
	background: `linear-gradient(to right, #3b82f6 ${fillPct.value}%, var(--a12) ${fillPct.value}%)`,
}));

function onInput(e: Event) {
	emit("update:modelValue", Number((e.target as HTMLInputElement).value));
}
</script>

<template>
	<div class="rc-slider-row">
		<div class="rc-slider-row__head">
			<span v-if="label" :id="labelId" class="rc-slider-row__label">{{ label }}</span>
			<span class="rc-slider-row__value">{{ displayValue }}</span>
		</div>
		<input
			type="range"
			class="rc-slider"
			:style="trackStyle"
			:min="min"
			:max="max"
			:step="1"
			:value="modelValue"
			:disabled="disabled"
			:aria-labelledby="label ? labelId : undefined"
			:aria-describedby="hint ? hintId : undefined"
			@input="onInput"
		/>
		<p v-if="hint" :id="hintId" class="rc-slider-row__hint">{{ hint }}</p>
	</div>
</template>

<style scoped>
.rc-slider-row {
	display: flex;
	flex-direction: column;
	gap: 6px;
}

.rc-slider-row__head {
	display: flex;
	justify-content: space-between;
	align-items: baseline;
}

.rc-slider-row__label {
	font-size: 12px;
	color: var(--t2);
}

.rc-slider-row__value {
	font-family: ui-monospace, monospace;
	font-size: 11px;
	color: var(--blue);
}

.rc-slider-row__hint {
	margin: 0;
	font-size: 10.5px;
	color: var(--t5);
	line-height: 1.45;
}

.rc-slider {
	appearance: none;
	width: 100%;
	height: 4px;
	border-radius: 2px;
	background: var(--a12);
	cursor: pointer;
}

.rc-slider:disabled {
	cursor: not-allowed;
	opacity: 0.5;
}

.rc-slider::-webkit-slider-thumb {
	appearance: none;
	width: 12px;
	height: 12px;
	border-radius: 50%;
	background: #fff;
	box-shadow: 0 1px 3px var(--shC);
}

.rc-slider::-moz-range-thumb {
	width: 12px;
	height: 12px;
	border: none;
	border-radius: 50%;
	background: #fff;
	box-shadow: 0 1px 3px var(--shC);
}

.rc-slider::-moz-range-progress {
	background: #3b82f6;
	height: 4px;
	border-radius: 2px;
}
</style>

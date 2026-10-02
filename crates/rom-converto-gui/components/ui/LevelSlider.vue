<script setup lang="ts">
const props = withDefaults(
	defineProps<{
		modelValue: number;
		min?: number;
		max?: number;
		label?: string;
		hint?: string;
		tooltip?: string;
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
	background: `linear-gradient(to right, var(--fill) ${fillPct.value}%, var(--a12) ${fillPct.value}%)`,
}));

function onInput(e: Event) {
	emit("update:modelValue", Number((e.target as HTMLInputElement).value));
}
</script>

<template>
	<div class="rc-slider-row">
		<div class="rc-slider-row__head">
			<FieldLabel v-if="label" :id="labelId" :label="label" :tooltip="tooltip" />
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
	padding: 6px 0;
}

.rc-slider-row__head {
	display: flex;
	justify-content: space-between;
	align-items: baseline;
	gap: 16px;
	margin-bottom: 6px;
}

.rc-slider-row__value {
	font-family: var(--font-mono);
	font-size: var(--fs-sm);
	white-space: nowrap;
	color: var(--blue);
}

.rc-slider-row__hint {
	margin: 4px 0 0;
	font-size: var(--fs-sm);
	color: var(--t5);
	line-height: var(--lh-body);
	text-wrap: pretty;
}

.rc-slider {
	appearance: none;
	width: 100%;
	height: 4px;
	border-radius: var(--r-sm);
	background: var(--a12);
	cursor: pointer;
	margin: 0;
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
	background: var(--fill);
	border: 2px solid #fff;
	box-shadow: 0 0 0 1px var(--a25);
}

.rc-slider::-moz-range-thumb {
	width: 12px;
	height: 12px;
	border: 2px solid #fff;
	border-radius: 50%;
	background: var(--fill);
	box-shadow: 0 0 0 1px var(--a25);
}

.rc-slider::-moz-range-progress {
	background: var(--fill);
	height: 4px;
	border-radius: var(--r-sm);
}
</style>

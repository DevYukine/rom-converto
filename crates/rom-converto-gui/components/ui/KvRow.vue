<script setup lang="ts">
withDefaults(
	defineProps<{
		label: string;
		value: string;
		clickable?: boolean;
		color?: "t3" | "blue" | "green" | "yellow";
		tooltip?: string;
	}>(),
	{ color: "t3" },
);

const emit = defineEmits<{
	click: [];
}>();
</script>

<template>
	<div class="rc-kv">
		<span class="rc-kv__label">{{ label }}</span>
		<button
			v-if="clickable"
			type="button"
			class="rc-kv__value rc-kv__value--clickable"
			:title="tooltip"
			@click="emit('click')"
		>
			{{ value }}
		</button>
		<span v-else class="rc-kv__value" :class="`rc-kv__value--${color}`" :title="tooltip">{{ value }}</span>
	</div>
</template>

<style scoped>
.rc-kv {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 10px;
	padding: 3px 0;
}

.rc-kv__label {
	color: var(--t2);
	font-size: 12px;
}

.rc-kv__value {
	font-family: ui-monospace, monospace;
	font-size: 11px;
	border: none;
	background: none;
	padding: 0;
	text-align: right;
}

.rc-kv__value--t3 {
	color: var(--t3);
}

.rc-kv__value--blue {
	color: var(--blue);
}

.rc-kv__value--green {
	color: var(--green);
}

.rc-kv__value--yellow {
	color: var(--yellow);
}

.rc-kv__value--clickable {
	color: var(--blue);
	cursor: pointer;
}

.rc-kv__value--clickable:hover {
	text-decoration: underline;
}
</style>

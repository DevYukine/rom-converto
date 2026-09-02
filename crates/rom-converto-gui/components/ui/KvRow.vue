<script setup lang="ts">
defineProps<{
	label: string;
	value: string;
	clickable?: boolean;
	color?: "t3" | "blue" | "green" | "yellow" | "red";
	tooltip?: string;
}>();

const emit = defineEmits<{
	click: [];
}>();
</script>

<template>
	<div class="rc-kv">
		<FieldLabel :label="label" :tooltip="tooltip" />
		<button
			v-if="clickable"
			type="button"
			class="rc-kv__value rc-kv__value--clickable"
			:class="color && `rc-kv__value--${color}`"
			@click="emit('click')"
		>
			{{ value }}
		</button>
		<span v-else class="rc-kv__value" :class="`rc-kv__value--${color ?? 't3'}`">{{ value }}</span>
	</div>
</template>

<style scoped>
.rc-kv {
	display: grid;
	grid-template-columns: minmax(0, max-content) minmax(0, 1fr);
	gap: 10px;
	align-items: start;
	padding: 3px 0;
}

.rc-kv__value {
	font-family: ui-monospace, monospace;
	font-size: 11px;
	border: none;
	background: none;
	padding: 0;
	text-align: right;
	min-width: 0;
	white-space: normal;
	overflow-wrap: anywhere;
}

.rc-kv__value--clickable {
	color: var(--blue);
	cursor: pointer;
}

.rc-kv__value--clickable:hover {
	text-decoration: underline;
}

/* Color variants come after --clickable so an explicit color wins over
   the clickable-blue default. */
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

.rc-kv__value--red {
	color: var(--red);
}
</style>

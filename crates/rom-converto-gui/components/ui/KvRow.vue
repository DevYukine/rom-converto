<script setup lang="ts">
withDefaults(
	defineProps<{
		label: string;
		value: string;
		clickable?: boolean;
		stacked?: boolean;
		placeholder?: string;
		icon?: "folder" | "edit" | "chevron";
		color?: "t3" | "blue" | "green" | "yellow" | "red";
		tooltip?: string;
	}>(),
	{ placeholder: "Not set", icon: "chevron" },
);

const emit = defineEmits<{
	click: [];
}>();
</script>

<template>
	<div class="rc-kv" :class="{ 'rc-kv--stacked': stacked }">
		<div class="rc-kv__label"><FieldLabel :label="label" :tooltip="tooltip" /></div>
		<button
			v-if="clickable"
			type="button"
			class="rc-picker rc-kv__picker"
			:title="value"
			:aria-label="`${label}: ${value || placeholder}`"
			@click="emit('click')"
		>
			<span v-if="value" class="rc-picker__value rc-kv__value" :class="`rc-kv__value--${color ?? 'blue'}`">{{ value }}</span>
			<span v-else class="rc-picker__value rc-kv__value rc-kv__value--empty">{{ placeholder }}</span>
			<svg class="rc-picker__icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
				<path v-if="icon === 'folder'" d="M3 7h6l2 2h10v10H3z" />
				<path v-else-if="icon === 'edit'" d="m16 3 5 5-12 12-6 1 1-6zM14 5l5 5" />
				<path v-else d="m6 9 6 6 6-6" />
			</svg>
		</button>
		<span v-else class="rc-kv__value" :class="value ? `rc-kv__value--${color ?? 't3'}` : 'rc-kv__value--empty'" :title="value">{{ value || placeholder }}</span>
	</div>
</template>

<style scoped>
.rc-kv {
	display: flex;
	flex-wrap: wrap;
	gap: 16px;
	align-items: center;
	min-height: 40px;
	padding: 6px 0;
}

.rc-kv__label {
	flex: 0 1 auto;
	min-width: 0;
}

.rc-kv__value {
	font-family: var(--font-mono);
	font-size: var(--fs-sm);
	text-align: right;
	min-width: 0;
	white-space: normal;
	overflow-wrap: anywhere;
}

/* Fill the space left of the right-aligned value so wrapped lines stay flush right. */
.rc-kv:not(.rc-kv--stacked) > .rc-kv__value {
	flex: 1 1 120px;
}

.rc-kv__picker {
	flex: none;
	min-width: 112px;
	max-width: 60%;
	margin-left: auto;
}

.rc-kv__picker .rc-kv__value {
	text-align: left;
	white-space: nowrap;
}

.rc-kv--stacked {
	flex-direction: column;
	align-items: stretch;
	gap: 6px;
}

.rc-kv--stacked .rc-kv__picker,
.rc-kv--stacked > .rc-kv__value {
	width: 100%;
	max-width: none;
	text-align: left;
}

.rc-kv__value--empty {
	font-family: inherit;
	color: var(--t6);
}

/* Explicit value colors also apply inside picker controls. */
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

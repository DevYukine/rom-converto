<script setup lang="ts">
const props = withDefaults(
	defineProps<{
		label: string;
		count?: number;
		color?: "neutral" | "green" | "yellow" | "red";
		active?: boolean;
	}>(),
	{ color: "neutral" },
);

const emit = defineEmits<{
	click: [];
}>();

const PALETTE: Record<string, { bg: string; text: string; ring: string }> = {
	neutral: { bg: "var(--a10)", text: "var(--t3)", ring: "var(--t5)" },
	green: { bg: "rgba(63,185,80,.15)", text: "var(--green)", ring: "var(--green)" },
	yellow: { bg: "rgba(210,153,34,.15)", text: "var(--yellow)", ring: "var(--yellow)" },
	red: { bg: "rgba(212,58,62,.15)", text: "var(--red)", ring: "var(--red)" },
};

const FALLBACK = { bg: "var(--a10)", text: "var(--t3)", ring: "var(--t5)" };
const colors = computed(() => PALETTE[props.color] ?? FALLBACK);
</script>

<template>
	<button
		type="button"
		class="rc-filter-chip"
		:class="{ 'rc-filter-chip--active': active }"
		:style="{ background: colors.bg, color: colors.text, '--ring-color': colors.ring }"
		@click="emit('click')"
	>
		{{ count !== undefined ? `${label} ${count}` : label }}
	</button>
</template>

<style scoped>
.rc-filter-chip {
	border: none;
	border-radius: 16px;
	padding: 5px 13px;
	font-size: 11.5px;
	cursor: pointer;
}

.rc-filter-chip--active {
	box-shadow: 0 0 0 1.5px var(--ring-color);
}
</style>

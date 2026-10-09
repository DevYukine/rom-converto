<script setup lang="ts">
const props = defineProps<{
	type: string;
}>();

const PALETTE: Record<string, { bg: string; fg: string }> = {
	game: { bg: "var(--tint-green)", fg: "var(--green)" },
	update: { bg: "var(--tint-blue)", fg: "var(--blue)" },
	dlc: { bg: "var(--tint-purple)", fg: "var(--purple)" },
	demo: { bg: "var(--tint-yellow)", fg: "var(--yellow)" },
};

const FALLBACK = { bg: "var(--a10)", fg: "var(--t3)" };
const c = computed(() => PALETTE[props.type.toLowerCase()] ?? FALLBACK);
</script>

<template>
	<span class="rc-content-chip" :style="{ background: c.bg, color: c.fg, borderColor: c.fg }">
		{{ type }}
	</span>
</template>

<style scoped>
.rc-content-chip {
	display: inline-flex;
	align-items: center;
	height: 20px;
	border: 1px solid;
	border-radius: var(--r-sm);
	padding: 0 8px;
	font-size: var(--fs-xs);
	font-weight: 600;
	white-space: nowrap;
	flex-shrink: 0;
}
</style>

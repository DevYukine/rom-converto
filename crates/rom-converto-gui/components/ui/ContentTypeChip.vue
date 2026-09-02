<script setup lang="ts">
const props = defineProps<{
	type: string;
}>();

const PALETTE: Record<string, { bg: string; fg: string }> = {
	game: { bg: "rgba(63,185,80,.15)", fg: "var(--green)" },
	update: { bg: "rgba(93,148,245,.16)", fg: "var(--blue)" },
	dlc: { bg: "rgba(163,113,247,.16)", fg: "var(--purple)" },
	demo: { bg: "rgba(210,153,34,.15)", fg: "var(--yellow)" },
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
	border: 1px solid;
	border-radius: 999px;
	padding: 2px 9px;
	font-size: 10px;
	font-weight: 700;
	letter-spacing: .4px;
	white-space: nowrap;
	flex-shrink: 0;
}
</style>

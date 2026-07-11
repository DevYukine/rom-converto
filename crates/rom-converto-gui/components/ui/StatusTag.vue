<script setup lang="ts">
const props = withDefaults(
	defineProps<{
		status: string;
		label?: string;
		width?: number;
	}>(),
	{ width: 70 },
);

const PALETTE: Record<string, { bg: string; text: string }> = {
	PASSED: { bg: "rgba(63,185,80,.15)", text: "var(--green)" },
	MATCHED: { bg: "rgba(63,185,80,.15)", text: "var(--green)" },
	VERIFIED: { bg: "rgba(63,185,80,.15)", text: "var(--green)" },
	RENAMED: { bg: "rgba(63,185,80,.15)", text: "var(--green)" },
	MISNAMED: { bg: "rgba(210,153,34,.15)", text: "var(--yellow)" },
	HINT: { bg: "rgba(210,153,34,.1)", text: "var(--yellow)" },
	UNKNOWN: { bg: "var(--a10)", text: "var(--t3)" },
	UNSUPPORTED: { bg: "var(--a10)", text: "var(--t3)" },
	FAILED: { bg: "rgba(212,58,62,.15)", text: "var(--red)" },
	MISMATCH: { bg: "rgba(212,58,62,.15)", text: "var(--red)" },
	BASE: { bg: "rgba(69,147,248,.15)", text: "var(--blue)" },
	UPDATE: { bg: "rgba(63,185,80,.15)", text: "var(--green)" },
	DLC: { bg: "rgba(210,153,34,.15)", text: "var(--yellow)" },
};

const FALLBACK = { bg: "var(--a10)", text: "var(--t3)" };
const colors = computed(() => PALETTE[props.status.toUpperCase()] ?? FALLBACK);
</script>

<template>
	<span class="rc-status-tag" :style="{ width: `${width}px`, background: colors.bg, color: colors.text }">
		{{ label ?? status }}
	</span>
</template>

<style scoped>
.rc-status-tag {
	display: inline-flex;
	justify-content: center;
	flex-shrink: 0;
	border-radius: 5px;
	padding: 2px 8px;
	font-size: 10px;
	font-weight: 600;
	text-align: center;
}
</style>

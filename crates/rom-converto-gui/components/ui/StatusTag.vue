<script setup lang="ts">
const props = defineProps<{
	status: string;
	label?: string;
}>();

const PALETTE: Record<string, { bg: string; text: string }> = {
	PASSED: { bg: "var(--tint-green)", text: "var(--green)" },
	MATCHED: { bg: "var(--tint-green)", text: "var(--green)" },
	VERIFIED: { bg: "var(--tint-green)", text: "var(--green)" },
	RENAMED: { bg: "var(--tint-green)", text: "var(--green)" },
	CHECKED: { bg: "var(--tint-blue)", text: "var(--blue)" },
	MISNAMED: { bg: "var(--tint-yellow)", text: "var(--yellow)" },
	HINT: { bg: "var(--tint-yellow)", text: "var(--yellow)" },
	UNKNOWN: { bg: "var(--a10)", text: "var(--t3)" },
	UNSUPPORTED: { bg: "var(--a10)", text: "var(--t3)" },
	FAILED: { bg: "var(--tint-red)", text: "var(--red)" },
	MISMATCH: { bg: "var(--tint-red)", text: "var(--red)" },
	UNVERIFIED: { bg: "var(--a10)", text: "var(--t3)" },
	BASE: { bg: "var(--tint-blue)", text: "var(--blue)" },
	UPDATE: { bg: "var(--tint-green)", text: "var(--green)" },
	DLC: { bg: "var(--tint-yellow)", text: "var(--yellow)" },
};

const FALLBACK = { bg: "var(--a10)", text: "var(--t3)" };
const colors = computed(() => PALETTE[props.status.toUpperCase()] ?? FALLBACK);
</script>

<template>
	<span class="rc-status-tag" :style="{ background: colors.bg, color: colors.text }">
		{{ label ?? status }}
	</span>
</template>

<style scoped>
.rc-status-tag {
	display: inline-flex;
	align-items: center;
	justify-content: center;
	flex-shrink: 0;
	box-sizing: border-box;
	width: 100%;
	height: 20px;
	border-radius: var(--r-sm);
	padding: 0 8px;
	font-size: var(--fs-xs);
	font-weight: 600;
	text-align: center;
	white-space: nowrap;
}
</style>

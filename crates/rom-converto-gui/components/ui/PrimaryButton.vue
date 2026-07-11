<script setup lang="ts">
withDefaults(
	defineProps<{
		variant?: "primary" | "destructive" | "outlined";
		disabled?: boolean;
		type?: "button" | "submit";
	}>(),
	{ variant: "primary", type: "button" },
);

defineEmits<{
	click: [MouseEvent];
}>();
</script>

<template>
	<button
		:type="type"
		:disabled="disabled"
		class="rc-btn"
		:class="`rc-btn--${variant}`"
		@click="(e: MouseEvent) => !disabled && $emit('click', e)"
	>
		<slot />
	</button>
</template>

<style scoped>
.rc-btn {
	border-radius: 9px;
	padding: 10px 22px;
	font-size: 13px;
	font-weight: 700;
	cursor: pointer;
	border: none;
}

.rc-btn:disabled {
	cursor: not-allowed;
}

.rc-btn--primary {
	background: #2f6fd0;
	color: #fff;
}

.rc-btn--primary:disabled {
	background: var(--btnDim);
}

.rc-btn--primary:not(:disabled):hover {
	background: #3b82f6;
}

.rc-btn--destructive {
	background: #d43a3e;
	color: #fff;
}

.rc-btn--destructive:not(:disabled):hover {
	background: #e04a4e;
}

.rc-btn--outlined {
	background: transparent;
	border: 1px solid var(--a18);
	color: var(--t3);
}

.rc-btn--outlined:not(:disabled):hover {
	border-color: var(--a40);
}

.rc-btn--outlined:disabled {
	opacity: 0.5;
}
</style>

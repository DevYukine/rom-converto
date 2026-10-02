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
	height: 32px;
	flex: none;
	border-radius: var(--r-md);
	padding: 0 16px;
	font-size: var(--fs-md);
	font-weight: 600;
	white-space: nowrap;
	cursor: pointer;
	border: none;
}

.rc-btn:disabled {
	cursor: not-allowed;
}

.rc-btn--primary {
	background: var(--fill);
	color: #fff;
}

.rc-btn--primary:disabled {
	background: var(--a08);
	color: var(--t5);
}

.rc-btn--primary:not(:disabled):hover {
	background: var(--fill-hover);
}

.rc-btn--destructive {
	background: var(--fill-danger);
	color: #fff;
}

.rc-btn--destructive:not(:disabled):hover {
	filter: brightness(1.08);
}

.rc-btn--outlined {
	background: transparent;
	border: 1px solid var(--a18);
	color: var(--t2);
}

.rc-btn--outlined:not(:disabled):hover {
	border-color: var(--a40);
}

.rc-btn--outlined:disabled {
	color: var(--t5);
	border-color: var(--a10);
}
</style>

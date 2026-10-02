<script setup lang="ts">
const { toasts } = useToast();
</script>

<template>
	<div class="host" aria-live="polite">
		<div v-for="t in toasts" :key="t.id" class="toast">
			<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--green)" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
				<path d="M5 13l4 4L19 7" />
			</svg>
			<span>{{ t.message }}</span>
		</div>
	</div>
</template>

<style scoped>
.host {
	position: fixed;
	left: 50%;
	bottom: calc(var(--queuebar-h) + 20px);
	transform: translateX(-50%);
	display: flex;
	flex-direction: column-reverse;
	align-items: center;
	gap: 8px;
	max-width: calc(100vw - 32px);
	max-height: calc(100vh - var(--titlebar-h) - var(--queuebar-h) - 32px);
	overflow-y: auto;
	z-index: 60;
	pointer-events: none;
}
.toast {
	display: flex;
	align-items: center;
	gap: 8px;
	max-width: calc(100vw - 32px);
	background: var(--pop2);
	border: 1px solid var(--a16);
	border-radius: var(--r-md);
	padding: 8px 18px;
	box-shadow: 0 12px 36px var(--shC);
	font-size: var(--fs-sm);
	color: var(--t1);
}
.toast svg {
	flex: none;
}
.toast span {
	min-width: 0;
	line-height: var(--lh-body);
	text-wrap: pretty;
	overflow-wrap: anywhere;
}
</style>

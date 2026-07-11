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
	bottom: 64px;
	transform: translateX(-50%);
	display: flex;
	flex-direction: column-reverse;
	align-items: center;
	gap: 8px;
	z-index: 60;
	pointer-events: none;
}
.toast {
	display: flex;
	align-items: center;
	gap: 8px;
	background: var(--pop2);
	border: 1px solid var(--a16);
	border-radius: 8px;
	padding: 8px 18px;
	box-shadow: 0 12px 36px var(--shC);
	font-size: 12px;
	color: var(--t1);
}
</style>

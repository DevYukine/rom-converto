<script setup lang="ts">
defineProps<{
	title: string;
}>();
</script>

<template>
	<div class="rc-config-card">
		<div class="rc-config-card__head">
			<span class="rc-config-card__title">{{ title }}</span>
			<slot name="head-tag" />
		</div>
		<div class="rc-config-card__body">
			<slot />
		</div>
	</div>
</template>

<style scoped>
.rc-config-card {
	border: 1px solid var(--a10);
	border-radius: 10px;
	background: var(--card);
	padding: 12px 14px;
}

.rc-config-card__head {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 8px;
	margin-bottom: 8px;
}

.rc-config-card__title {
	font-size: 10.5px;
	font-weight: 700;
	text-transform: uppercase;
	letter-spacing: 0.8px;
	color: var(--t4);
}

.rc-config-card__body {
	display: flex;
	flex-direction: column;
	gap: 4px;
}
</style>

<script setup lang="ts">
defineProps<{ title: string }>();
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
	border-radius: var(--r-lg);
	background: var(--card);
	padding: 14px 16px;
	min-width: 0;
}

.rc-config-card__head {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 8px;
	margin-bottom: 8px;
}

.rc-config-card__title {
	min-width: 0;
	font-size: var(--fs-lg);
	font-weight: 600;
	color: var(--t1);
	white-space: nowrap;
}

.rc-config-card__body {
	display: flex;
	flex-direction: column;
}
</style>

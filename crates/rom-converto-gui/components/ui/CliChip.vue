<script setup lang="ts">
const props = defineProps<{
	command: string;
}>();

const emit = defineEmits<{
	copy: [text: string];
}>();

async function copy() {
	try {
		await navigator.clipboard.writeText(props.command);
	} catch {
		// clipboard unavailable (permission denied or no secure context); nothing to fall back to.
	}
	emit("copy", props.command);
}
</script>

<template>
	<button type="button" class="rc-cli-chip" :title="`Copy command: ${command}`" :aria-label="`Copy command: ${command}`" @click="copy">
		<span class="rc-cli-chip__prompt" aria-hidden="true">$</span>
		<span class="rc-cli-chip__command">{{ command }}</span>
		<svg class="rc-cli-chip__copy" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" aria-hidden="true">
			<rect x="8" y="8" width="12" height="12" rx="2" />
			<path d="M16 8V4H4v12h4" />
		</svg>
	</button>
</template>

<style scoped>
.rc-cli-chip {
	display: inline-flex;
	align-items: center;
	gap: 8px;
	min-width: 0;
	height: var(--ctl-h);
	font-family: var(--font-mono);
	font-size: var(--fs-xs);
	color: var(--t4);
	background: var(--bg2);
	border: 1px solid var(--a14);
	border-radius: var(--r-sm);
	padding: 0 10px;
	cursor: pointer;
	white-space: nowrap;
	overflow: hidden;
	max-width: min(640px, 100%);
}

.rc-cli-chip:hover {
	border-color: var(--a30);
	color: var(--t3);
}

.rc-cli-chip__command {
	min-width: 0;
	overflow: hidden;
	text-overflow: ellipsis;
}

.rc-cli-chip__prompt {
	color: var(--t6);
	flex: none;
}

.rc-cli-chip__copy {
	flex: none;
}
</style>

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
	<button type="button" class="rc-cli-chip" title="Click to copy" @click="copy">
		{{ command }}<span class="rc-cli-chip__ellipsis">…</span>
	</button>
</template>

<style scoped>
.rc-cli-chip {
	font-family: ui-monospace, monospace;
	font-size: 10.5px;
	color: var(--t4);
	background: var(--bg2);
	border: 1px solid var(--a14);
	border-radius: 6px;
	padding: 5px 10px;
	cursor: pointer;
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
}

.rc-cli-chip:hover {
	border-color: var(--a30);
	color: var(--t3);
}

.rc-cli-chip__ellipsis {
	opacity: 0.7;
}
</style>

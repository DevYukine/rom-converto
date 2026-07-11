<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from "vue";
import { isTauri, open } from "~/lib/ipc";
import { registerDropZone, unregisterDropZone } from "~/composables/useDragDrop";

const props = defineProps<{
	dropText: string;
	filters?: { name: string; extensions: string[] }[];
	multiple?: boolean;
	directory?: boolean;
	// Show a folder picker next to the file picker.
	alsoDirectory?: boolean;
}>();

const emit = defineEmits<{ add: [paths: string[]] }>();

const el = ref<HTMLElement | null>(null);
let zoneId: string | null = null;

onMounted(() => {
	if (isTauri && el.value) {
		zoneId = registerDropZone(el.value, (paths) => emit("add", paths), 10);
	}
});

onBeforeUnmount(() => {
	if (zoneId) unregisterDropZone(zoneId);
});

async function browse(directory: boolean) {
	const picked = await open({
		multiple: props.multiple ?? false,
		directory,
		filters: directory ? undefined : props.filters,
	});
	if (Array.isArray(picked)) emit("add", picked);
	else if (typeof picked === "string") emit("add", [picked]);
}
</script>

<template>
	<div ref="el" class="rc-drop">
		<svg class="rc-drop__icon" width="20" height="20" viewBox="0 0 24 24" fill="none"
			stroke="#4593f8" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
			<path d="M4 4h5l2 3h9v11a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z" />
		</svg>
		<span class="rc-drop__text">{{ dropText }}</span>
		<button type="button" class="rc-drop__browse" @click="browse(directory ?? false)">
			{{ directory ? "Browse folder" : "Browse" }}
		</button>
		<button v-if="!directory && alsoDirectory" type="button" class="rc-drop__browse" @click="browse(true)">
			Browse folder
		</button>
	</div>
</template>

<style scoped>
.rc-drop {
	display: flex;
	align-items: center;
	gap: 10px;
	border: 1.5px dashed var(--a22);
	border-radius: 10px;
	padding: 12px 14px;
	color: var(--t4);
	font-size: 12px;
}

.rc-drop:hover,
.rc-drop.drop-hover {
	border-color: #4593f8;
}

.rc-drop__icon {
	flex-shrink: 0;
}

.rc-drop__text {
	flex: 1;
	min-width: 0;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-drop__browse {
	border: 1px solid var(--a25);
	border-radius: 6px;
	padding: 4px 12px;
	font-size: 11px;
	color: var(--t0);
	font-weight: 500;
	background: transparent;
	cursor: pointer;
}
</style>

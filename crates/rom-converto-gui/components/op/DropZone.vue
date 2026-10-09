<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from "vue";
import { isTauri, open } from "~/lib/ipc";
import { registerDropZone, unregisterDropZone } from "~/composables/useDragDrop";

const props = defineProps<{
	dropText: string;
	filters?: { name: string; extensions: string[] }[];
	fileLabel?: string;
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
		<div class="rc-drop__copy">
			<svg class="rc-drop__icon" width="20" height="20" viewBox="0 0 24 24" fill="none"
				stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<path d="M4 4h5l2 3h9v11a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z" />
			</svg>
			<span class="rc-drop__text">{{ dropText }}</span>
		</div>
		<div class="rc-drop__actions">
			<button type="button" class="rc-drop__browse" @click="browse(directory ?? false)">
				{{ directory ? "Browse folder" : (fileLabel ?? "Browse") }}
			</button>
			<button v-if="!directory && alsoDirectory" type="button" class="rc-drop__browse" @click="browse(true)">
				Browse folder
			</button>
		</div>
	</div>
</template>

<style scoped>
.rc-drop {
	display: flex;
	flex-wrap: wrap;
	align-items: center;
	gap: 10px;
	min-height: 64px;
	border: 1px dashed var(--a18);
	border-radius: var(--r-lg);
	padding: 14px 16px;
	color: var(--t3);
	font-size: var(--fs-md);
}

.rc-drop:hover,
.rc-drop.drop-hover {
	border-color: var(--blue);
}

.rc-drop__copy {
	display: flex;
	align-items: center;
	flex: 1 1 340px;
	gap: 10px;
	min-width: 0;
}

.rc-drop__icon {
	flex: none;
	color: var(--blue);
}

.rc-drop__text {
	flex: 1 1 auto;
	min-width: 0;
	line-height: var(--lh-body);
	text-wrap: pretty;
}

.rc-drop__actions {
	display: flex;
	flex: none;
	gap: 10px;
	max-width: 100%;
}

.rc-drop__browse {
	flex: none;
	height: 32px;
	border: 1px solid var(--a18);
	border-radius: var(--r-md);
	padding: 0 16px;
	font-size: var(--fs-md);
	color: var(--t2);
	font-weight: 600;
	background: transparent;
	white-space: nowrap;
	cursor: pointer;
}

.rc-drop__browse:hover {
	border-color: var(--a40);
}
</style>

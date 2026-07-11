<script setup lang="ts">
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke, isTauri } from "~/lib/ipc";

const version = ref("");

onMounted(async () => {
	version.value = await invoke<string>("app_display_version");
});

function minimize() {
	if (isTauri) getCurrentWindow().minimize();
}

function toggleMaximize() {
	if (isTauri) getCurrentWindow().toggleMaximize();
}

function close() {
	if (isTauri) getCurrentWindow().close();
}
</script>

<template>
	<div class="titlebar" data-tauri-drag-region>
		<div class="left" data-tauri-drag-region>
			<div class="icon" />
			<span class="name">rom-converto</span>
			<span v-if="version" class="version">v{{ version }}</span>
		</div>
		<div class="controls">
			<button
				type="button"
				class="control"
				aria-label="Minimize window"
				@click="minimize"
			>
				&#9472;
			</button>
			<button
				type="button"
				class="control"
				aria-label="Maximize window"
				@click="toggleMaximize"
			>
				&#9634;
			</button>
			<button
				type="button"
				class="control"
				aria-label="Close window"
				@click="close"
			>
				&#10005;
			</button>
		</div>
	</div>
</template>

<style scoped>
.titlebar {
	display: flex;
	align-items: center;
	justify-content: space-between;
	height: 36px;
	padding: 0 12px 0 14px;
	background: var(--bg2);
	border-bottom: 1px solid var(--a10);
}

.left {
	display: flex;
	align-items: center;
	gap: 8px;
}

.icon {
	width: 16px;
	height: 16px;
	border-radius: 4px;
	background: #2f6fd0;
}

.name {
	font-size: 12px;
	font-weight: 600;
	color: var(--t1);
}

.version {
	font-family: ui-monospace, monospace;
	font-size: 10.5px;
	color: var(--t5);
}

.controls {
	display: flex;
	align-items: center;
	gap: 14px;
}

.control {
	background: none;
	border: none;
	padding: 0;
	color: var(--t5);
	font-size: 12px;
	line-height: 1;
	cursor: pointer;
}

.control:hover {
	color: var(--t1);
}
</style>

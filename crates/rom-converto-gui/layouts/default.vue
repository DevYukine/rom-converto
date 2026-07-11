<script setup lang="ts">
import { useUiStore } from "~/stores/ui";
import { useQueueStore } from "~/stores/queue";
import { useConfigStore } from "~/stores/config";
import { isDraggingOver } from "~/composables/useDragDrop";

useUiStore();
const queue = useQueueStore();
const config = useConfigStore();
const route = useRoute();

onMounted(() => {
	if (!config.loaded) config.loadConfig();
});

const CONTEXT_OPS = new Set([
	"compress",
	"extract",
	"verify",
	"decrypt",
	"encrypt",
	"convert",
	"dat",
	"tools",
]);

const currentOp = computed(() => route.path.split("/")[1] ?? "");
const showContext = computed(() => CONTEXT_OPS.has(currentOp.value));

const alertsOpen = ref(false);
</script>

<template>
	<div class="app">
		<Titlebar />

		<div class="mid">
			<IconRail :alerts-open="alertsOpen" @toggle-alerts="alertsOpen = !alertsOpen" />
			<ContextPanel v-if="showContext" :op="currentOp" />
			<main class="content"><slot /></main>
		</div>

		<QueueDrawer v-if="queue.drawerOpen" />
		<QueueBar />

		<AlertsFlyout v-if="alertsOpen" @close="alertsOpen = false" />
		<ToastHost />

		<div v-if="isDraggingOver" class="drop-overlay">
			<span>Release to load</span>
		</div>
	</div>
</template>

<style scoped>
.app {
	position: relative;
	display: flex;
	flex-direction: column;
	height: 100vh;
	background: var(--bg);
	color: var(--t1);
	font-size: 13px;
	user-select: none;
}
.mid {
	display: flex;
	flex: 1;
	min-height: 0;
}
.content {
	flex: 1;
	min-width: 0;
	overflow-y: auto;
}
.drop-overlay {
	position: absolute;
	inset: 0;
	z-index: 60;
	display: flex;
	align-items: center;
	justify-content: center;
	background: var(--overlay);
	border: 2px dashed #4593f8;
	border-radius: 10px;
	pointer-events: none;
	font-size: 15px;
	font-weight: 600;
	color: var(--t0);
}
</style>

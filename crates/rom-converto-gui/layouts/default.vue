<script setup lang="ts">
import { useUiStore } from "~/stores/ui";
import { useQueueStore } from "~/stores/queue";
import { useConfigStore } from "~/stores/config";
import { useUpdatesStore } from "~/stores/updates";
import { isDraggingOver } from "~/composables/useDragDrop";

useUiStore();
const queue = useQueueStore();
const config = useConfigStore();
const updates = useUpdatesStore();
const route = useRoute();

onMounted(() => {
	if (!config.loaded) config.loadConfig();
	updates.start();
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
			<div class="workspace">
				<div class="main-row">
					<ContextPanel v-if="showContext" :op="currentOp" />
					<main class="content"><slot /></main>
				</div>
				<QueueDrawer v-if="queue.drawerOpen" />
			</div>
		</div>

		<QueueBar />

		<AlertsFlyout v-if="alertsOpen" @close="alertsOpen = false" />
		<ToastHost />
		<UpdateToast />
		<ContextMenu />

		<div v-if="isDraggingOver" class="drop-overlay">
			<span>Release to load</span>
		</div>
	</div>
</template>

<style scoped>
.app {
	--titlebar-h: 36px;
	--queuebar-h: 44px;
	--rail-w: 72px;
	position: relative;
	display: flex;
	flex-direction: column;
	height: 100vh;
	background: var(--bg);
	color: var(--t1);
	font-size: var(--fs-md);
	user-select: none;
}
.mid {
	display: flex;
	flex: 1;
	min-height: 0;
}
.workspace {
	display: flex;
	flex-direction: column;
	flex: 1;
	min-width: 0;
	min-height: 0;
}
.main-row {
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
	border: 2px dashed var(--blue);
	border-radius: var(--r-lg);
	pointer-events: none;
	font-size: var(--fs-lg);
	font-weight: 600;
	color: var(--t0);
}
@media (max-height: 719px) {
	.app {
		--rail-w: 56px;
	}
}
</style>

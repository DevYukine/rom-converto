<script setup lang="ts">
import { useAlertsStore, type Alert, type AlertAction } from "~/stores/alerts";

const emit = defineEmits<{ close: [] }>();

const store = useAlertsStore();
function onKeydown(event: KeyboardEvent) {
	if (event.defaultPrevented || event.key !== "Escape") return;
	event.preventDefault();
	emit("close");
}

onMounted(() => document.addEventListener("keydown", onKeydown));
onBeforeUnmount(() => document.removeEventListener("keydown", onKeydown));

function onAction(a: Alert, act: AlertAction) {
	store.markRead(a.id);
	act.run?.();
	emit("close");
}

const BORDER: Record<Alert["type"], string> = {
	error: "color-mix(in srgb, var(--red) 40%, transparent)",
	ok: "color-mix(in srgb, var(--green) 35%, transparent)",
	plain: "var(--a10)",
};
const TITLE_COLOR: Record<Alert["type"], string> = {
	error: "var(--red)",
	ok: "var(--green)",
	plain: "var(--t2)",
};
</script>

<template>
	<div class="wrap">
		<div class="backdrop" @click="emit('close')" />
		<div class="panel">
			<div class="head">
				<span class="htitle">Alerts</span>
				<div class="hactions">
					<button type="button" @click="store.markAllRead()">Mark all read</button>
					<button type="button" @click="store.clear()">Clear</button>
					<button type="button" aria-label="Close alerts" @click="emit('close')">✕</button>
				</div>
			</div>

			<p v-if="!store.alerts.length" class="empty">No alerts.</p>

			<div class="list">
				<div
					v-for="a in store.alerts"
					:key="a.id"
					class="card"
					:style="{ borderColor: BORDER[a.type], opacity: a.unread ? 1 : 0.8 }"
				>
					<div class="ctitle" :style="{ color: TITLE_COLOR[a.type] }">
						<span>{{ a.title }}</span>
						<span v-if="a.unread" class="dot" />
					</div>
					<p class="cbody">{{ a.body }}</p>
					<div v-if="a.actions?.length" class="cactions">
						<template v-for="(act, i) in a.actions" :key="i">
							<span v-if="i > 0" class="sep">·</span>
							<button type="button" class="act-btn" @click="onAction(a, act)">{{ act.label }}</button>
						</template>
					</div>
					<div class="cmeta">{{ a.meta }}</div>
				</div>
			</div>

			<p class="footer">
				OS notifications still fire when the window is unfocused. This panel keeps the history.
			</p>
		</div>
	</div>
</template>

<style scoped>
.backdrop {
	position: absolute;
	inset: var(--titlebar-h) 0 0;
	z-index: 19;
}
.panel {
	position: absolute;
	left: var(--rail-w);
	top: var(--titlebar-h);
	bottom: var(--queuebar-h);
	width: min(360px, calc(100vw - var(--rail-w) - 24px));
	background: var(--bg);
	border-right: 1px solid var(--a12);
	padding: 16px 14px;
	box-shadow: 12px 0 40px var(--shC);
	z-index: 20;
	overflow-y: auto;
	display: flex;
	flex-direction: column;
}
.head {
	display: flex;
	align-items: center;
	justify-content: space-between;
	margin-bottom: 12px;
}
.htitle {
	font-size: var(--fs-lg);
	font-weight: 700;
	color: var(--t0);
}
.hactions {
	display: flex;
	align-items: center;
	gap: 4px;
	white-space: nowrap;
}
.hactions button {
	height: var(--ctl-h);
	padding: 0 6px;
	border-radius: var(--r-sm);
	background: none;
	border: none;
	color: var(--t4);
	font-size: var(--fs-sm);
	cursor: pointer;
}
.hactions button:hover {
	background: var(--a08);
	color: var(--t0);
}
.list {
	display: flex;
	flex-direction: column;
	gap: 10px;
}
.card {
	border: 1px solid var(--a10);
	border-radius: var(--r-lg);
	background: var(--card);
	padding: 11px 12px;
}
.ctitle {
	display: flex;
	align-items: center;
	justify-content: space-between;
	font-size: var(--fs-md);
	font-weight: 600;
}
.dot {
	width: 7px;
	height: 7px;
	border-radius: 50%;
	background: var(--blue);
}
.cbody {
	margin: 6px 0 0;
	font-size: var(--fs-sm);
	color: var(--t3);
	line-height: var(--lh-body);
	text-wrap: pretty;
	overflow-wrap: anywhere;
}
.cactions {
	display: flex;
	align-items: center;
	flex-wrap: wrap;
	gap: 5px;
	margin-top: 6px;
	font-size: var(--fs-sm);
	font-weight: 700;
	color: var(--blue);
}
.act-btn {
	background: none;
	border: none;
	padding: 0;
	color: var(--blue);
	font-size: var(--fs-sm);
	font-weight: 700;
	cursor: pointer;
	white-space: nowrap;
}
.sep {
	color: var(--blue);
}
.empty {
	margin: 0;
	font-size: var(--fs-md);
	color: var(--t6);
}
.cmeta {
	margin-top: 6px;
	font-size: var(--fs-xs);
	color: var(--t5);
}
.footer {
	margin-top: auto;
	padding-top: 12px;
	font-size: var(--fs-sm);
	color: var(--t6);
	line-height: var(--lh-body);
	text-wrap: pretty;
}
</style>

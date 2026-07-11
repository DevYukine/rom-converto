<script setup lang="ts">
import { useAlertsStore } from "~/stores/alerts";
import { useUiStore } from "~/stores/ui";
import { opDef } from "~/lib/opdefs";

defineProps<{ alertsOpen: boolean }>();
const emit = defineEmits<{ toggleAlerts: [] }>();

const alerts = useAlertsStore();
const ui = useUiStore();
const route = useRoute();
const router = useRouter();

const NAV: { op: string; label: string; icon: string }[] = [
	{ op: "compress", label: "Compress", icon: "M4 14h6v6M20 10h-6V4M14 10l7-7M3 21l7-7" },
	{ op: "extract", label: "Extract", icon: "M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7" },
	{ op: "verify", label: "Verify", icon: "M12 2l8 3.5V11c0 5-3.4 8.6-8 11-4.6-2.4-8-6-8-11V5.5zM9 11.5l2 2 4-4.5" },
	{ op: "decrypt", label: "Decrypt", icon: "M4 12a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v5a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2zM13 10V7a4 4 0 0 1 8 0v3" },
	{ op: "encrypt", label: "Encrypt", icon: "M5 12a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2zM8 10V7a4 4 0 0 1 8 0v3M12 14.4a1.1 1.1 0 1 0 0 2.2 1.1 1.1 0 0 0 0-2.2" },
	{ op: "convert", label: "Convert", icon: "M17 2l4 4-4 4M3 11v-1a4 4 0 0 1 4-4h14M7 22l-4-4 4-4M21 13v1a4 4 0 0 1-4 4H3" },
	{ op: "inspect", label: "Inspect", icon: "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14M21 21l-4.35-4.35" },
	{ op: "dat", label: "DAT", icon: "M4 5c0-1.66 3.58-3 8-3s8 1.34 8 3-3.58 3-8 3-8-1.34-8-3M4 5v14c0 1.66 3.58 3 8 3s8-1.34 8-3V5M4 12c0 1.66 3.58 3 8 3s8-1.34 8-3" },
	{ op: "tools", label: "Tools", icon: "M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1.5 14h5M9.5 12h5M17.5 16h5" },
	{ op: "queue", label: "Queue", icon: "M12 2l9 5-9 5-9-5zM3 12l9 5 9-5M3 17l9 5 9-5" },
];

const BELL = "M6 9a6 6 0 1 1 12 0c0 5 2 6 2 6H4s2-1 2-6M10 19a2 2 0 0 0 4 0";
const GEAR =
	"M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M18.4 5.6l-2.1 2.1M7.7 16.3l-2.1 2.1";

const DEFAULT_CONSOLE: Record<string, string> = {
	compress: "nx",
	extract: "ctr",
	verify: "ctr",
	decrypt: "ctr",
	encrypt: "ctr",
	convert: "ctr",
	dat: "scan",
	tools: "playlist",
};

const currentOp = computed(() => route.path.split("/")[1] ?? "");

function go(op: string) {
	if (op === "inspect" || op === "queue") {
		router.push(`/${op}`);
		return;
	}
	const last = ui.lastConsolePerOp[op];
	// dat pages live outside the opdef registry; a stale persisted console
	// (e.g. one removed from an op) must not route to a dead page.
	const valid = op === "dat" ? ["scan", "verify", "rename"].includes(last ?? "") : !!(last && opDef(op, last));
	router.push(`/${op}/${valid ? last : DEFAULT_CONSOLE[op]}`);
}
</script>

<template>
	<nav class="rail">
		<button
			v-for="item in NAV"
			:key="item.op"
			type="button"
			class="item"
			:class="{ active: currentOp === item.op }"
			:aria-label="item.label"
			@click="go(item.op)"
		>
			<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<path :d="item.icon" />
			</svg>
			<span class="label">{{ item.label }}</span>
		</button>

		<div class="spacer" />

		<button
			type="button"
			class="item"
			:class="{ active: alertsOpen }"
			aria-label="Alerts"
			@click="emit('toggleAlerts')"
		>
			<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<path :d="BELL" />
			</svg>
			<span class="label">Alerts</span>
			<span v-if="alerts.unreadCount > 0" class="badge">{{ alerts.unreadCount }}</span>
		</button>

		<button
			type="button"
			class="item"
			:class="{ active: currentOp === 'settings' }"
			aria-label="Settings"
			@click="router.push('/settings')"
		>
			<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<path :d="GEAR" />
			</svg>
			<span class="label">Settings</span>
		</button>
	</nav>
</template>

<style scoped>
.rail {
	display: flex;
	flex-direction: column;
	align-items: center;
	width: 72px;
	padding: 12px 0;
	background: var(--bg2);
	border-right: 1px solid var(--a10);
	gap: 4px;
	overflow-y: auto;
}
.item {
	position: relative;
	flex-shrink: 0;
	display: flex;
	flex-direction: column;
	align-items: center;
	gap: 4px;
	width: 60px;
	padding: 8px 0;
	border: none;
	border-radius: 9px;
	background: transparent;
	color: var(--t4);
	font-size: 10.5px;
	font-weight: 400;
	cursor: pointer;
}
.item:hover {
	background: var(--a08);
}
.item.active {
	color: var(--t0);
	background: var(--a12);
	font-weight: 600;
}
.label {
	line-height: 1;
}
.spacer {
	flex: 1;
}
.badge {
	position: absolute;
	top: 4px;
	right: 10px;
	min-width: 15px;
	height: 15px;
	padding: 0 3px;
	border-radius: 8px;
	background: #d43a3e;
	color: #fff;
	font-size: 9px;
	font-weight: 700;
	display: flex;
	align-items: center;
	justify-content: center;
}
</style>

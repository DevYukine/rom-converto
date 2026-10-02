<script setup lang="ts">
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { useUpdatesStore } from "~/stores/updates";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";

const updates = useUpdatesStore();

const state = toRef(updates, "state");
const busy = computed(() => state.value.phase === "downloading" || state.value.phase === "installing");
const blocked = toRef(updates, "blocked");
const percent = computed(() => Math.round(Math.max(state.value.progress, 0) * 100));
const progressText = computed(() => {
	if (state.value.phase === "installing") return "Installing, the app restarts in a moment.";
	return state.value.progress < 0 ? "Downloading…" : `Downloading ${percent.value}%`;
});

function openNotes() {
	void openExternal(`https://github.com/DevYukine/rom-converto/releases/tag/v${state.value.availableVersion}`);
}
</script>

<template>
	<Transition name="slide">
		<div v-if="updates.open" class="toast" role="status" aria-live="polite">
			<div class="head">
				<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
					<path d="M12 19V5M5 12l7-7 7 7" />
				</svg>
				<span class="title">
					<template v-if="state.phase === 'error'">Update failed</template>
					<template v-else-if="busy">Updating to v{{ state.availableVersion }}</template>
					<template v-else>Update available</template>
				</span>
				<button v-if="!busy" type="button" class="close" aria-label="Dismiss" @click="updates.later()">✕</button>
			</div>

			<p v-if="state.phase === 'error'" class="body">{{ state.error }}</p>
			<p v-else-if="busy" class="body">{{ progressText }}</p>
			<p v-else class="body">
				v{{ state.availableVersion }} is ready to install.
				<button type="button" class="link" @click="openNotes">What's new</button>
			</p>

			<div v-if="busy" class="bar" :class="{ indeterminate: state.progress < 0 || state.phase === 'installing' }">
				<div class="fill" :style="state.phase === 'downloading' && state.progress >= 0 ? { width: `${percent}%` } : undefined" />
			</div>

			<div v-else class="actions">
				<template v-if="state.phase === 'error'">
					<PrimaryButton :disabled="blocked" @click="updates.retry()">Try again</PrimaryButton>
					<PrimaryButton variant="outlined" @click="updates.later()">Dismiss</PrimaryButton>
				</template>
				<template v-else>
					<PrimaryButton :disabled="blocked" @click="updates.install()">Install and restart</PrimaryButton>
					<PrimaryButton variant="outlined" @click="updates.later()">Later</PrimaryButton>
					<button type="button" class="link skip" @click="updates.skip()">Skip this version</button>
				</template>
			</div>
			<p v-if="blocked && !busy" class="caption">Available once running jobs finish or are cancelled.</p>
		</div>
	</Transition>
</template>

<style scoped>
.toast {
	position: fixed;
	right: 16px;
	bottom: calc(var(--queuebar-h) + 16px);
	z-index: 55;
	width: min(340px, calc(100vw - 32px));
	max-height: calc(100vh - var(--titlebar-h) - var(--queuebar-h) - 32px);
	overflow-y: auto;
	padding: 12px 14px;
	background: var(--pop2);
	border: 1px solid var(--a16);
	border-radius: var(--r-lg);
	box-shadow: 0 12px 36px var(--shC);
	color: var(--t1);
	font-size: var(--fs-sm);
}
.head {
	display: flex;
	align-items: center;
	gap: 8px;
	color: var(--blue);
}
.title {
	flex: 1;
	min-width: 0;
	font-size: var(--fs-md);
	text-wrap: balance;
	font-weight: 600;
	color: var(--t0);
}
.close {
	background: none;
	border: none;
	color: var(--t4);
	cursor: pointer;
	font-size: var(--fs-xs);
	padding: 2px 4px;
}
.close:hover {
	color: var(--t1);
}
.body {
	margin: 6px 0 0;
	color: var(--t2);
	line-height: var(--lh-body);
	text-wrap: pretty;
	word-break: break-word;
}
.link {
	background: none;
	border: none;
	padding: 0;
	color: var(--blue);
	cursor: pointer;
	font-size: inherit;
	white-space: nowrap;
}
.link:hover {
	text-decoration: underline;
}
.actions {
	display: flex;
	flex-wrap: wrap;
	align-items: center;
	gap: 8px;
	margin-top: 10px;
}
.skip {
	margin-left: auto;
	color: var(--t4);
}
.caption {
	margin: 6px 0 0;
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	text-wrap: pretty;
	color: var(--t5);
}
.bar {
	margin-top: 10px;
	height: 4px;
	border-radius: var(--r-sm);
	background: var(--a10);
	overflow: hidden;
}
.fill {
	height: 100%;
	background: var(--blue);
	transition: width 0.2s;
}
.indeterminate .fill {
	width: 40%;
	animation: sweep 1.2s ease-in-out infinite;
}
@keyframes sweep {
	from {
		transform: translateX(-100%);
	}
	to {
		transform: translateX(250%);
	}
}
.slide-enter-active,
.slide-leave-active {
	transition: opacity 0.18s, transform 0.18s;
}
.slide-enter-from,
.slide-leave-to {
	opacity: 0;
	transform: translateY(8px);
}
</style>

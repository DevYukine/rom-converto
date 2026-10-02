<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useConfigStore } from "~/stores/config";
import { PRESET_BINDINGS as BINDINGS } from "~/lib/preset-bindings";
import type { Preset, PresetFormat } from "~/types";

const props = defineProps<{ console: string }>();

const config = useConfigStore();
if (!config.loaded) config.loadConfig();

const open = ref(false);
let restoreFocus = false;
const triggerEl = ref<HTMLElement | null>(null);
const popoverEl = ref<HTMLElement | null>(null);
const popoverPos = ref({ left: 16, top: 16 });

async function positionPopover() {
	if (!open.value) return;
	await nextTick();
	if (!triggerEl.value || !popoverEl.value) return;
	const trigger = triggerEl.value.getBoundingClientRect();
	const popover = popoverEl.value.getBoundingClientRect();
	popoverPos.value = {
		left: Math.max(16, Math.min(trigger.left, window.innerWidth - popover.width - 16)),
		top: Math.max(16, Math.min(trigger.top - popover.height - 8, window.innerHeight - popover.height - 16)),
	};
}

let resizeObserver: ResizeObserver | undefined;
let observedPopover: HTMLElement | null = null;

function onOutsideClick(event: MouseEvent) {
	const target = event.target as Node;
	if (!popoverEl.value?.contains(target) && !triggerEl.value?.contains(target)) open.value = false;
}

// Buttons in the popover and the trigger use `mousedown.prevent` because WebKit
// does not focus a clicked button: the blur would otherwise land here with no
// relatedTarget and unmount the popover before the click arrives.
function onFocusout(event: FocusEvent) {
	const target = event.relatedTarget as Node | null;
	if (!popoverEl.value?.contains(target) && !triggerEl.value?.contains(target)) open.value = false;
}

function onKeydown(event: KeyboardEvent) {
	if (event.key !== "Escape") return;
	event.preventDefault();
	restoreFocus = true;
	open.value = false;
}

function removeOpenListeners() {
	document.removeEventListener("click", onOutsideClick);
	document.removeEventListener("keydown", onKeydown, true);
	document.removeEventListener("scroll", positionPopover, true);
}

watch(open, async (isOpen) => {
	removeOpenListeners();
	if (observedPopover) resizeObserver?.unobserve(observedPopover);
	observedPopover = null;
	if (!isOpen) {
		if (restoreFocus) {
			restoreFocus = false;
			await nextTick();
			triggerEl.value?.focus({ preventScroll: true });
		}
		return;
	}
	restoreFocus = false;
	document.addEventListener("click", onOutsideClick);
	document.addEventListener("keydown", onKeydown, true);
	document.addEventListener("scroll", positionPopover, true);
	await positionPopover();
	if (!open.value || !popoverEl.value) return;
	observedPopover = popoverEl.value;
	resizeObserver?.observe(observedPopover);
	(popoverEl.value.querySelector<HTMLElement>('[aria-pressed="true"]') ?? popoverEl.value.querySelector("button"))?.focus();
});

onMounted(() => {
	window.addEventListener("resize", positionPopover);
	resizeObserver = new ResizeObserver(positionPopover);
	if (triggerEl.value) {
		resizeObserver.observe(triggerEl.value);
		const panel = triggerEl.value.closest("aside");
		if (panel) resizeObserver.observe(panel);
	}
});
onBeforeUnmount(() => {
	removeOpenListeners();
	window.removeEventListener("resize", positionPopover);
	resizeObserver?.disconnect();
});
const current = computed(() => config.activePreset || "None");
const names = computed(() => Object.keys(config.presets).sort());

function isSetValue(value: unknown): boolean {
	if (value === null || value === undefined || value === "" || value === false) return false;
	return !Array.isArray(value) || value.length > 0;
}
const SUMMARY_LABELS: Record<string, string> = {
	level: "Level",
	mode: "Mode",
	block_size_exp: "Block size",
	block_size: "Block size",
	chunk_size: "Chunk size",
	hunk_size: "Hunk size",
	codecs: "Codecs",
	on_conflict: "On conflict",
	output_dir: "Output folder",
	report: "Run report",
};

function summary(name: string): string {
	const binding = BINDINGS[props.console];
	const preset = config.presets[name];
	if (!binding || !preset) return "";
	const table = preset[binding.format] as Record<string, unknown> | null | undefined;
	if (!table) return "No settings for this console";
	const parts: string[] = [];
	for (const key of Object.keys(binding.map)) {
		const value = table[key];
		if (isSetValue(value)) parts.push(`${SUMMARY_LABELS[key] ?? key}: ${value}`);
	}
	return parts.join(" · ") || "Defaults";
}

function applyToStore(name: string): void {
	const binding = BINDINGS[props.console];
	if (!binding) return;
	const table = config.presets[name]?.[binding.format] as Record<string, unknown> | null | undefined;
	if (!table) return;
	const store = binding.useStore();
	for (const [key, field] of Object.entries(binding.map)) {
		const value = table[key];
		if (value !== null && value !== undefined) store[field] = value;
	}
}

function select(name: string | null) {
	config.applyPreset(name);
	if (name) applyToStore(name);
	restoreFocus = true;
	open.value = false;
}

const saveName = ref("");
const saving = ref(false);
const saveError = ref("");
// ctr/cue have no preset table; hide the save row where saving is a no-op.
const canSave = computed(() => !!BINDINGS[props.console]);

async function saveCurrent() {
	const trimmed = saveName.value.trim();
	if (!trimmed) return;
	const binding = BINDINGS[props.console];
	if (!binding) return;
	saving.value = true;
	saveError.value = "";
	try {
		const store = binding.useStore();
		const table: Record<string, string | number | string[]> = {};
		for (const [key, field] of Object.entries(binding.map)) {
			const value = store[field];
			if (isSetValue(value)) table[key] = value;
		}
		const preset: Preset = {
			...config.presets[trimmed],
			[binding.format]: table as Preset[PresetFormat],
		};
		await config.savePreset(trimmed, preset);
		config.applyPreset(trimmed);
		saveName.value = "";
		restoreFocus = true;
		open.value = false;
	} catch (e) {
		saveError.value = String(e);
	} finally {
		saving.value = false;
	}
}
</script>

<template>
	<div class="preset-wrap">
		<Teleport to="body">
		<div v-if="open" ref="popoverEl" class="popover" tabindex="-1" :style="{ left: `${popoverPos.left}px`, top: `${popoverPos.top}px` }" @focusout="onFocusout">
			<button type="button" class="pop-row" :aria-pressed="!config.activePreset" @mousedown.prevent @click="select(null)">
				<span>None</span><span class="pop-dim">(page defaults)</span>
				<svg v-if="!config.activePreset" class="pop-check" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="m5 13 4 4 10-10" /></svg>
			</button>
			<button v-for="name in names" :key="name" type="button" class="pop-row col" :aria-pressed="config.activePreset === name" :title="name" @mousedown.prevent @click="select(name)">
				<span class="pop-title">
					<span class="pop-strong">{{ name }}</span>
					<svg v-if="config.activePreset === name" class="pop-check" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="m5 13 4 4 10-10" /></svg>
				</span>
				<span class="pop-sub" :title="summary(name)">{{ summary(name) }}</span>
			</button>
			<div v-if="canSave" class="pop-save">
				<input
					v-model="saveName"
					type="text"
					class="pop-input rc-input"
					placeholder="Save current as…"
					@keydown.enter="saveCurrent"
				/>
				<button type="button" class="pop-save-btn" :disabled="!saveName.trim() || saving" @mousedown.prevent @click="saveCurrent">
					{{ saving ? "…" : "Save" }}
				</button>
			</div>
			<p v-if="saveError" class="pop-error">{{ saveError }}</p>
		</div>
		</Teleport>
		<button ref="triggerEl" type="button" class="preset rc-picker" :title="current" :aria-expanded="open" @mousedown.prevent @click="open = !open">
			<span class="p-label">Preset</span>
			<span class="p-value rc-picker__value">{{ current }}</span>
			<svg class="rc-picker__icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6" /></svg>
		</button>
	</div>
</template>

<style scoped>
.preset-wrap {
	flex: none;
}
.preset {
	width: 100%;
}
.p-label {
	flex: none;
	color: var(--t3);
	font-size: var(--fs-sm);
}
.p-value {
	color: var(--t0);
	font-weight: 600;
	font-size: var(--fs-sm);
	text-align: right;
}
.popover {
	position: fixed;
	width: min(300px, calc(100vw - 32px));
	max-height: min(320px, calc(100vh - 96px));
	overflow-y: auto;
	z-index: 30;
	background: var(--pop);
	border: 1px solid var(--a16);
	border-radius: var(--r-md);
	box-shadow: 0 12px 36px var(--shC);
	padding: 4px;
	display: flex;
	flex-direction: column;
	gap: 2px;
}
.pop-row {
	display: flex;
	align-items: center;
	gap: 6px;
	padding: 7px 10px;
	border: none;
	border-radius: var(--r-sm);
	background: transparent;
	cursor: pointer;
	text-align: left;
	color: var(--t3);
	font-size: var(--fs-sm);
	white-space: nowrap;
}
.pop-row:hover {
	background: var(--a08);
}
.pop-row.col {
	flex-direction: column;
	align-items: flex-start;
	gap: 2px;
	min-width: 0;
}
.pop-title {
	display: flex;
	align-items: center;
	gap: 8px;
	width: 100%;
	min-width: 0;
}
.pop-check {
	flex: none;
	margin-left: auto;
	color: var(--blue);
}
.pop-dim {
	color: var(--t5);
}
.pop-strong {
	color: var(--t0);
	font-weight: 600;
	min-width: 0;
	max-width: 100%;
	overflow: hidden;
	text-overflow: ellipsis;
}
.pop-sub {
	display: -webkit-box;
	max-width: 100%;
	overflow: hidden;
	text-overflow: ellipsis;
	-webkit-line-clamp: 2;
	-webkit-box-orient: vertical;
	font-size: var(--fs-xs);
	line-height: var(--lh-body);
	white-space: normal;
	overflow-wrap: anywhere;
	color: var(--t4);
}
.pop-save {
	display: flex;
	gap: 6px;
	padding: 6px 4px 2px;
	border-top: 1px solid var(--a08);
	margin-top: 2px;
}
.pop-input {
	flex: 1;
	min-width: 0;
	height: var(--ctl-h);
}
.pop-save-btn {
	flex: none;
	height: var(--ctl-h);
	font-size: var(--fs-sm);
	color: var(--t2);
	background: transparent;
	border: 1px solid var(--a14);
	border-radius: var(--r-sm);
	padding: 0 10px;
	white-space: nowrap;
	cursor: pointer;
}
.pop-save-btn:hover:not(:disabled) {
	border-color: var(--a40);
	color: var(--t0);
}
.pop-save-btn:disabled {
	opacity: 0.5;
	cursor: default;
}
.pop-error {
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	overflow-wrap: anywhere;
	color: var(--red);
	padding: 4px;
	margin: 0;
}
</style>

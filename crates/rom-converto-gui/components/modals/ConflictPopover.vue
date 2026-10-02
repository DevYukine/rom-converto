<script setup lang="ts">
import { nextTick, onBeforeUnmount, ref, watch } from "vue";

const props = withDefaults(
	defineProps<{
		modelValue: string;
		renameDisabled?: boolean;
	}>(),
	{ renameDisabled: false },
);

const emit = defineEmits<{
	"update:modelValue": [value: string];
}>();

const OPTIONS = [
	{ label: "Error", value: "error", description: "Refuse to write and stop." },
	{ label: "Overwrite", value: "overwrite", description: "Replace the existing output." },
	{
		label: "Skip",
		value: "skip",
		description: "Leave the existing output and move on, counted as skipped.",
	},
	{
		label: "Rename",
		value: "rename",
		description: "Write to the next free numbered name, so Game.chd becomes Game (1).chd.",
	},
	{
		label: "Overwrite if invalid",
		value: "overwrite-invalid",
		description:
			"Check the existing output, keep it if it passes or cannot be checked, rewrite it only if it fails.",
	},
];

const labels: Record<string, string> = Object.fromEntries(OPTIONS.map((o) => [o.value, o.label]));

const open = ref(false);
const root = ref<HTMLElement | null>(null);
const triggerEl = ref<HTMLElement | null>(null);
const popEl = ref<HTMLElement | null>(null);
const position = ref({ left: "16px", top: "16px", maxHeight: "calc(100vh - 32px)" });

function place() {
	if (!triggerEl.value || !popEl.value) return;
	const rect = triggerEl.value.getBoundingClientRect();
	const width = popEl.value.getBoundingClientRect().width;
	const below = Math.max(0, window.innerHeight - rect.bottom - 22);
	const above = Math.max(0, rect.top - 22);
	const useAbove = below < popEl.value.scrollHeight && above > below;
	const available = Math.max(0, useAbove ? above : below);
	const height = Math.min(popEl.value.scrollHeight + 2, available);
	const left = Math.max(16, Math.min(rect.right - width, window.innerWidth - width - 16));
	const top = useAbove ? rect.top - height - 6 : rect.bottom + 6;
	position.value = { left: `${left}px`, top: `${Math.max(16, Math.min(top, window.innerHeight - height - 16))}px`, maxHeight: `${available}px` };
}

function isDisabled(value: string) {
	return props.renameDisabled && value === "rename";
}

function optionLabel(value: string) {
	return isDisabled(value) ? `${labels[value]} not available` : labels[value];
}

function toggle() {
	open.value = !open.value;
}

function select(value: string) {
	if (isDisabled(value)) return;
	emit("update:modelValue", value);
	close();
}

function close(restoreFocus = true) {
	open.value = false;
	if (restoreFocus) triggerEl.value?.focus();
}

function onDocClick(e: MouseEvent) {
	if (!root.value?.contains(e.target as Node) && !popEl.value?.contains(e.target as Node)) close();
}

function onKeydown(e: KeyboardEvent) {
	if (e.key === "Escape") {
		e.stopPropagation();
		close();
	}
}

// Buttons in the popover and the trigger use `mousedown.prevent` because WebKit
// does not focus a clicked button: the blur would otherwise land here with no
// relatedTarget and unmount the popover before the click arrives.
function onFocusout(e: FocusEvent) {
	const next = e.relatedTarget as Node | null;
	if (!popEl.value?.contains(next) && !triggerEl.value?.contains(next)) close(false);
}

function onResize() {
	close(false);
}

function onScroll() {
	const rect = triggerEl.value?.getBoundingClientRect();
	if (!rect) return;
	const container = triggerEl.value?.closest("main")?.getBoundingClientRect();
	if (rect.bottom <= Math.max(0, container?.top ?? 0) || rect.top >= Math.min(window.innerHeight, container?.bottom ?? window.innerHeight)) {
		close(false);
	} else {
		place();
	}
}

watch(open, async (isOpen) => {
	if (isOpen) {
		document.addEventListener("mousedown", onDocClick);
		document.addEventListener("keydown", onKeydown);
		await nextTick();
		place();
		if (!open.value) return;
		window.addEventListener("resize", onResize);
		window.addEventListener("scroll", onScroll, true);
		const checked = popEl.value?.querySelector<HTMLElement>("button[aria-pressed='true']:not([disabled])");
		const first = popEl.value?.querySelector<HTMLElement>("button:not([disabled])");
		(checked ?? first)?.focus();
	} else {
		document.removeEventListener("mousedown", onDocClick);
		document.removeEventListener("keydown", onKeydown);
		window.removeEventListener("resize", onResize);
		window.removeEventListener("scroll", onScroll, true);
	}
});

onBeforeUnmount(() => {
	document.removeEventListener("mousedown", onDocClick);
	document.removeEventListener("keydown", onKeydown);
	window.removeEventListener("resize", onResize);
	window.removeEventListener("scroll", onScroll, true);
});
</script>

<template>
	<div ref="root" class="rc-conflict">
		<button
			ref="triggerEl"
			type="button"
			class="rc-picker"
			:title="optionLabel(modelValue)"
			:aria-expanded="open"
			@mousedown.prevent
			@click="toggle"
		>
			<span class="rc-picker__value">{{ optionLabel(modelValue) }}</span>
			<svg class="rc-picker__icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="m6 9 6 6 6-6" /></svg>
		</button>
		<Teleport to="body">
		<div v-if="open" ref="popEl" class="rc-pop" role="group" tabindex="-1" :style="position" @focusout="onFocusout">
			<button
				v-for="opt in OPTIONS"
				:key="opt.value"
				type="button"
				:aria-pressed="opt.value === modelValue"
				class="rc-opt"
				:class="{ active: opt.value === modelValue }"
				:disabled="isDisabled(opt.value)"
				@mousedown.prevent
				@click="select(opt.value)"
			>
				<span class="rc-opt-label">{{ optionLabel(opt.value) }}</span>
				<span class="rc-opt-desc">{{ opt.description }}</span>
			</button>
		</div>
		</Teleport>
	</div>
</template>

<style scoped>
.rc-conflict {
	display: inline-flex;
	flex: none;
	min-width: 0;
	max-width: 60%;
}

.rc-pop {
	position: fixed;
	box-sizing: border-box;
	width: min(260px, calc(100vw - 32px));
	overflow-y: auto;
	background: var(--pop);
	border: 1px solid var(--a16);
	border-radius: var(--r-md);
	box-shadow: 0 12px 36px var(--shC);
	padding: 4px;
	display: flex;
	flex-direction: column;
	z-index: 40;
}

.rc-opt {
	display: flex;
	flex-direction: column;
	gap: 2px;
	background: none;
	border: none;
	text-align: left;
	padding: 6px 8px;
	border-radius: var(--r-sm);
	color: var(--t3);
	font-size: var(--fs-md);
	cursor: pointer;
}

.rc-opt-label {
	white-space: nowrap;
}

.rc-opt-desc {
	font-size: var(--fs-sm);
	color: var(--t5);
	line-height: var(--lh-body);
	text-wrap: pretty;
}

.rc-opt:hover:not(:disabled) {
	background: var(--a08);
}

.rc-opt.active {
	background: var(--tint-blue);
	color: var(--t0);
}

.rc-opt:disabled {
	color: var(--t7);
	cursor: default;
	opacity: .6;
}
</style>

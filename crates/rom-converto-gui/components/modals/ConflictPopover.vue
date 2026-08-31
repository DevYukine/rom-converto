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
			"Check the existing output, keep it if it passes, rewrite it if it fails or cannot be checked.",
	},
];

const labels: Record<string, string> = Object.fromEntries(OPTIONS.map((o) => [o.value, o.label]));

const open = ref(false);
const root = ref<HTMLElement | null>(null);
const triggerEl = ref<HTMLElement | null>(null);

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

function close() {
	open.value = false;
	triggerEl.value?.focus();
}

function onDocClick(e: MouseEvent) {
	if (root.value && !root.value.contains(e.target as Node)) close();
}

function onKeydown(e: KeyboardEvent) {
	if (e.key === "Escape") {
		e.stopPropagation();
		close();
	}
}

watch(open, async (isOpen) => {
	if (isOpen) {
		document.addEventListener("mousedown", onDocClick);
		document.addEventListener("keydown", onKeydown);
		await nextTick();
		const first = root.value?.querySelector<HTMLElement>("[role='menuitemradio']:not([disabled])");
		first?.focus();
	} else {
		document.removeEventListener("mousedown", onDocClick);
		document.removeEventListener("keydown", onKeydown);
	}
});

onBeforeUnmount(() => {
	document.removeEventListener("mousedown", onDocClick);
	document.removeEventListener("keydown", onKeydown);
});
</script>

<template>
	<div ref="root" class="rc-conflict">
		<button
			ref="triggerEl"
			type="button"
			class="rc-trigger"
			aria-haspopup="true"
			:aria-expanded="open"
			@click="toggle"
		>
			{{ optionLabel(modelValue) }} <span class="rc-caret">▾</span>
		</button>
		<div v-if="open" class="rc-pop" role="menu">
			<button
				v-for="opt in OPTIONS"
				:key="opt.value"
				type="button"
				role="menuitemradio"
				:aria-checked="opt.value === modelValue"
				class="rc-opt"
				:class="{ active: opt.value === modelValue }"
				:disabled="isDisabled(opt.value)"
				@click="select(opt.value)"
			>
				<span class="rc-opt-label">{{ optionLabel(opt.value) }}</span>
				<span class="rc-opt-desc">{{ opt.description }}</span>
			</button>
		</div>
	</div>
</template>

<style scoped>
.rc-conflict {
	position: relative;
	display: inline-block;
}

.rc-trigger {
	background: none;
	border: none;
	color: var(--t2);
	font-family: ui-monospace, monospace;
	font-size: 11px;
	cursor: pointer;
	padding: 0;
}

.rc-caret {
	color: var(--t5);
}

.rc-pop {
	position: absolute;
	right: 0;
	top: 26px;
	width: 260px;
	background: var(--pop);
	border: 1px solid var(--a16);
	border-radius: 8px;
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
	border-radius: 6px;
	color: var(--t3);
	font-size: 12px;
	cursor: pointer;
}

.rc-opt-desc {
	font-size: 10.5px;
	color: var(--t5);
}

.rc-opt:hover:not(:disabled) {
	background: var(--a08);
}

.rc-opt.active {
	background: rgba(69, 147, 248, .14);
	color: var(--t0);
}

.rc-opt:disabled {
	color: var(--t7);
	cursor: default;
	opacity: .6;
}
</style>

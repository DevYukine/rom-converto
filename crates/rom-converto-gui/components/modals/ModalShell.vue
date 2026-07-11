<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from "vue";

withDefaults(
	defineProps<{
		title: string;
		width?: number;
	}>(),
	{ width: 520 },
);

const emit = defineEmits<{ close: [] }>();

const root = ref<HTMLElement | null>(null);
let trigger: HTMLElement | null = null;

const FOCUSABLE =
	'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

function focusables(): HTMLElement[] {
	if (!root.value) return [];
	return Array.from(root.value.querySelectorAll<HTMLElement>(FOCUSABLE));
}

function onKeydown(e: KeyboardEvent) {
	if (e.key === "Escape") {
		e.stopPropagation();
		emit("close");
		return;
	}
	if (e.key !== "Tab") return;
	const els = focusables();
	if (els.length === 0) {
		e.preventDefault();
		return;
	}
	const first = els[0]!;
	const last = els[els.length - 1]!;
	if (e.shiftKey && document.activeElement === first) {
		e.preventDefault();
		last.focus();
	} else if (!e.shiftKey && document.activeElement === last) {
		e.preventDefault();
		first.focus();
	}
}

onMounted(() => {
	trigger = document.activeElement as HTMLElement | null;
	const els = focusables();
	(els[0] ?? root.value)?.focus();
});

onBeforeUnmount(() => {
	trigger?.focus?.();
});
</script>

<template>
	<div class="rc-overlay" @click="emit('close')">
		<div
			ref="root"
			class="rc-modal"
			:style="{ width: `${width}px` }"
			role="dialog"
			aria-modal="true"
			:aria-label="title"
			tabindex="-1"
			@click.stop
			@keydown="onKeydown"
		>
			<div class="rc-header">
				<span class="rc-title">{{ title }}</span>
				<slot name="header-extra" />
				<button type="button" class="rc-close" aria-label="Close" @click="emit('close')">✕</button>
			</div>
			<div class="rc-body">
				<slot />
			</div>
			<div v-if="$slots.footer" class="rc-footer">
				<slot name="footer" />
			</div>
		</div>
	</div>
</template>

<style scoped>
.rc-overlay {
	position: fixed;
	inset: 0;
	background: var(--overlay);
	z-index: 50;
	display: flex;
	align-items: center;
	justify-content: center;
}

.rc-modal {
	background: var(--card);
	border: 1px solid var(--a16);
	border-radius: 12px;
	box-shadow: 0 24px 80px var(--shC);
	max-height: 80vh;
	display: flex;
	flex-direction: column;
}

.rc-header {
	display: flex;
	align-items: center;
	gap: 10px;
	padding: 12px 16px;
	border-bottom: 1px solid var(--a10);
}

.rc-title {
	font-size: 13.5px;
	font-weight: 700;
	color: var(--t0);
	white-space: nowrap;
}

.rc-close {
	margin-left: auto;
	background: none;
	border: none;
	color: var(--t5);
	font-size: 13px;
	cursor: pointer;
	padding: 2px 4px;
}

.rc-close:hover {
	color: var(--t0);
}

.rc-body {
	padding: 16px;
	overflow-y: auto;
}

.rc-footer {
	display: flex;
	align-items: center;
	gap: 10px;
	padding: 12px 16px;
	border-top: 1px solid var(--a10);
}
</style>

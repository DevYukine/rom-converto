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
// A drag that starts inside the dialog and ends on the backdrop must not close it.
let overlayPressed = false;

function onOverlayDown(e: MouseEvent) {
	overlayPressed = e.target === e.currentTarget;
}

function onOverlayClick() {
	if (overlayPressed) emit("close");
}

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
	const bodyControl = root.value?.querySelector<HTMLElement>(`.rc-body :is(${FOCUSABLE})`);
	const footerButton = root.value?.querySelector<HTMLElement>(".rc-footer .rc-btn--primary:not([disabled])")
		?? root.value?.querySelector<HTMLElement>(".rc-footer button:not([disabled])");
	(bodyControl ?? footerButton ?? root.value)?.focus();
});

onBeforeUnmount(() => {
	trigger?.focus?.();
});
</script>

<template>
	<Teleport to="body">
		<div class="rc-overlay" @mousedown="onOverlayDown" @click.self="onOverlayClick">
			<div
				ref="root"
				class="rc-modal"
				:style="{ width: `min(${width}px, calc(100vw - 32px))` }"
				role="dialog"
				aria-modal="true"
				:aria-label="title"
				tabindex="-1"
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
	</Teleport>
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
	padding: 24px 16px;
}

.rc-modal {
	background: var(--card);
	border: 1px solid var(--a16);
	border-radius: var(--r-lg);
	box-shadow: 0 24px 80px var(--shC);
	max-height: calc(100vh - 48px);
	min-width: 0;
	display: flex;
	flex-direction: column;
}

.rc-header {
	display: flex;
	align-items: center;
	flex: none;
	gap: 10px;
	padding: 12px 16px;
	border-bottom: 1px solid var(--a10);
}

.rc-title {
	min-width: 0;
	font-size: var(--fs-lg);
	font-weight: 600;
	color: var(--t0);
	text-wrap: balance;
	overflow-wrap: anywhere;
}

.rc-close {
	flex: none;
	margin-left: auto;
	background: none;
	border: none;
	border-radius: var(--r-sm);
	color: var(--t5);
	font-size: var(--fs-md);
	cursor: pointer;
	width: var(--ctl-h);
	height: var(--ctl-h);
	white-space: nowrap;
}

.rc-close:hover {
	color: var(--t0);
}

.rc-body {
	min-height: 0;
	padding: 16px;
	font-size: var(--fs-md);
	line-height: var(--lh-body);
	overflow-y: auto;
	overflow-wrap: anywhere;
}

.rc-footer {
	display: flex;
	flex: none;
	flex-wrap: wrap;
	align-items: center;
	gap: 10px;
	padding: 12px 16px;
	border-top: 1px solid var(--a10);
}

.rc-footer :deep(button) {
	flex: none;
	white-space: nowrap;
}
</style>

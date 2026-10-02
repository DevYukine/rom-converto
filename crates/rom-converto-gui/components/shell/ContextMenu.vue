<script setup lang="ts">
import { nextTick, onBeforeUnmount, ref, watch } from "vue";
import { useContextMenu, closeContextMenu } from "~/composables/useContextMenu";
import { useToast } from "~/composables/useToast";

const { open, x, y, items } = useContextMenu();
const { show: showToast } = useToast();

const menuEl = ref<HTMLElement | null>(null);
const posX = ref(0);
const posY = ref(0);

async function select(value: string) {
	try {
		await navigator.clipboard.writeText(value);
		showToast("Copied");
	} catch {
		// clipboard unavailable (permission denied or no secure context); nothing to fall back to.
	}
	closeContextMenu();
}

function onDocPointerDown(e: PointerEvent) {
	if (menuEl.value && !menuEl.value.contains(e.target as Node)) closeContextMenu();
}

function onKeydown(e: KeyboardEvent) {
	if (e.key === "Escape") closeContextMenu();
}

function onScroll() {
	closeContextMenu();
}

function removeListeners() {
	document.removeEventListener("pointerdown", onDocPointerDown);
	document.removeEventListener("keydown", onKeydown);
	document.removeEventListener("scroll", onScroll, true);
	window.removeEventListener("resize", onScroll);
}

watch(open, async (isOpen) => {
	if (isOpen) {
		posX.value = x.value;
		posY.value = y.value;
		document.addEventListener("pointerdown", onDocPointerDown);
		document.addEventListener("keydown", onKeydown);
		document.addEventListener("scroll", onScroll, true);
		window.addEventListener("resize", onScroll);
		await nextTick();
		const el = menuEl.value;
		if (el) {
			const rect = el.getBoundingClientRect();
			if (posX.value + rect.width > window.innerWidth) posX.value = window.innerWidth - rect.width - 4;
			if (posY.value + rect.height > window.innerHeight) posY.value = window.innerHeight - rect.height - 4;
			posX.value = Math.max(4, posX.value);
			posY.value = Math.max(4, posY.value);
		}
	} else {
		removeListeners();
	}
});

onBeforeUnmount(removeListeners);
</script>

<template>
	<div
		v-if="open"
		ref="menuEl"
		class="rc-ctx-menu"
		role="menu"
		:style="{ left: `${posX}px`, top: `${posY}px` }"
		@contextmenu.prevent
	>
		<button
			v-for="item in items"
			:key="item.label"
			type="button"
			role="menuitem"
			class="rc-ctx-menu__item"
			@click="select(item.value)"
			:title="item.label"
		>
			{{ item.label }}
		</button>
	</div>
</template>

<style scoped>
.rc-ctx-menu {
	position: fixed;
	min-width: 160px;
	max-width: calc(100vw - 32px);
	max-height: calc(100vh - 32px);
	overflow-y: auto;
	background: var(--pop);
	border: 1px solid var(--a16);
	border-radius: var(--r-md);
	box-shadow: 0 12px 36px var(--shC);
	padding: 4px;
	display: flex;
	flex-direction: column;
	z-index: 80;
}

.rc-ctx-menu__item {
	background: none;
	border: none;
	text-align: left;
	padding: 6px 8px;
	border-radius: var(--r-sm);
	color: var(--t3);
	font-size: var(--fs-sm);
	cursor: pointer;
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
}

.rc-ctx-menu__item:hover {
	background: var(--a08);
}
</style>

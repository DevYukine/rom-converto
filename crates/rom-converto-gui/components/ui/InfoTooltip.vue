<script setup lang="ts">
// `label` names the icon trigger; the message itself is linked as its description.
defineProps<{ message: string; label: string }>();

const tipId = useId();
const trigger = ref<HTMLElement | null>(null);
const bubble = ref<HTMLElement | null>(null);
const hovered = ref(false);
const focused = ref(false);
const open = computed(() => hovered.value || focused.value);
const position = ref({ left: "8px", top: "8px" });

function place() {
	if (!trigger.value || !bubble.value) return;
	const rect = trigger.value.getBoundingClientRect();
	const tip = bubble.value.getBoundingClientRect();
	const left = Math.max(8, Math.min(rect.left + (rect.width - tip.width) / 2, window.innerWidth - tip.width - 8));
	const above = rect.top - tip.height - 6;
	const top = Math.max(8, Math.min(
		above >= 8 && above + tip.height <= window.innerHeight - 8 ? above : rect.bottom + 6,
		window.innerHeight - tip.height - 8,
	));
	position.value = { left: `${left}px`, top: `${top}px` };
}

watch(open, async (value) => {
	if (value) {
		await nextTick();
		place();
		window.addEventListener("resize", place);
		window.addEventListener("scroll", place, true);
	} else {
		window.removeEventListener("resize", place);
		window.removeEventListener("scroll", place, true);
	}
});

onBeforeUnmount(() => {
	window.removeEventListener("resize", place);
	window.removeEventListener("scroll", place, true);
});
</script>

<template>
	<span
		ref="trigger"
		class="rc-info-tooltip"
		tabindex="0"
		role="img"
		:aria-label="label"
		:aria-describedby="tipId"
		@mouseenter="hovered = true"
		@mouseleave="hovered = false"
		@focusin="focused = true"
		@focusout="focused = false"
	>
		<slot />
		<span :id="tipId" hidden>{{ message }}</span>
		<Teleport to="body">
			<span v-if="open" ref="bubble" role="tooltip" aria-hidden="true" class="rc-info-tooltip__bubble" :style="position">
				{{ message }}
			</span>
		</Teleport>
	</span>
</template>

<style scoped>
.rc-info-tooltip {
	display: inline-flex;
}

.rc-info-tooltip__bubble {
	position: fixed;
	z-index: 100;
	pointer-events: none;
	box-sizing: border-box;
	width: max-content;
	max-width: min(280px, calc(100vw - 16px));
	max-height: calc(100vh - 16px);
	overflow: auto;
	background: var(--pop2);
	border: 1px solid var(--a16);
	color: var(--t2);
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	padding: 6px 9px;
	border-radius: var(--r-sm);
	box-shadow: 0 6px 24px var(--shC);
	white-space: normal;
	text-wrap: pretty;
}
</style>

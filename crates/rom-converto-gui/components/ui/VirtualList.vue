<script setup lang="ts" generic="T">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

// Fixed-height rows windowed against the nearest scrolling ancestor, so the
// page keeps one natural scrollbar while only the visible rows exist in the DOM.
const props = defineProps<{
	items: T[];
	rowHeight: number;
	keyOf: (item: T) => string;
}>();

const OVERSCAN = 8;

const root = ref<HTMLElement | null>(null);
const start = ref(0);
const end = ref(0);
let parent: HTMLElement | null = null;
let scroller: HTMLElement | Window | null = null;
let raf = 0;
let observer: ResizeObserver | null = null;

function scrollParent(el: HTMLElement): HTMLElement | null {
	let node = el.parentElement;
	while (node) {
		const { overflowY } = getComputedStyle(node);
		if (overflowY === "auto" || overflowY === "scroll") return node;
		node = node.parentElement;
	}
	return null;
}

function update() {
	raf = 0;
	if (!root.value || !parent) return;
	const offset = parent.getBoundingClientRect().top - root.value.getBoundingClientRect().top;
	const viewport = parent.clientHeight;
	start.value = Math.max(0, Math.floor(offset / props.rowHeight) - OVERSCAN);
	end.value = Math.min(props.items.length, Math.ceil((offset + viewport) / props.rowHeight) + OVERSCAN);
}

function schedule() {
	raf ||= requestAnimationFrame(update);
}

onMounted(() => {
	if (!root.value) return;
	parent = scrollParent(root.value) ?? document.documentElement;
	// Document scrolling fires on window, not on the root element.
	scroller = parent === document.documentElement ? window : parent;
	scroller.addEventListener("scroll", schedule, { passive: true });
	observer = new ResizeObserver(schedule);
	observer.observe(parent);
	observer.observe(root.value);
	update();
});

onBeforeUnmount(() => {
	scroller?.removeEventListener("scroll", schedule);
	observer?.disconnect();
	if (raf) cancelAnimationFrame(raf);
});

watch(() => props.items, schedule);

const slice = computed(() => props.items.slice(start.value, end.value));
</script>

<template>
	<div ref="root" class="rc-vlist" :style="{ height: `${items.length * rowHeight}px` }">
		<div class="rc-vlist__window" :style="{ transform: `translateY(${start * rowHeight}px)` }">
			<div v-for="item in slice" :key="keyOf(item)" :style="{ height: `${rowHeight}px` }">
				<slot :item="item" />
			</div>
		</div>
	</div>
</template>

<style scoped>
.rc-vlist {
	position: relative;
}

.rc-vlist__window {
	position: absolute;
	inset: 0 0 auto;
	will-change: transform;
}
</style>

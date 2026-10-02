<script setup lang="ts" generic="T">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

// Rows are windowed against the nearest scrolling ancestor, so the page keeps
// one natural scrollbar while only the visible rows exist in the DOM.
const props = defineProps<{
	items: T[];
	rowHeight: number | ((item: T) => number);
	keyOf: (item: T, index: number) => string;
}>();

const OVERSCAN = 8;

const root = ref<HTMLElement | null>(null);
const start = ref(0);
const end = ref(0);
let parent: HTMLElement | null = null;
let scroller: HTMLElement | Window | null = null;
let raf = 0;
let observer: ResizeObserver | null = null;

const offsets = computed(() => {
	const height = props.rowHeight;
	if (typeof height === "number") return null;
	const out = [0];
	for (const item of props.items) out.push(out[out.length - 1]! + height(item));
	return out;
});

function offsetAt(index: number): number {
	const positions = offsets.value;
	return positions ? positions[Math.min(index, props.items.length)]! : index * (props.rowHeight as number);
}

function indexAt(offset: number): number {
	const positions = offsets.value;
	if (!positions) return Math.floor(offset / (props.rowHeight as number));
	let lo = 0;
	let hi = props.items.length;
	while (lo < hi) {
		const mid = Math.ceil((lo + hi) / 2);
		if (positions[mid]! <= offset) lo = mid;
		else hi = mid - 1;
	}
	return lo;
}

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
	start.value = Math.min(props.items.length, Math.max(0, indexAt(offset) - OVERSCAN));
	end.value = Math.min(props.items.length, Math.max(0, indexAt(offset + viewport) + 1 + OVERSCAN));
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

watch([() => props.items, () => props.rowHeight], schedule);

const slice = computed(() => props.items.slice(start.value, end.value));
</script>

<template>
	<div ref="root" class="rc-vlist" :style="{ height: `${offsetAt(items.length)}px` }">
		<div class="rc-vlist__window" :style="{ transform: `translateY(${offsetAt(start)}px)` }">
			<div v-for="(item, index) in slice" :key="keyOf(item, start + index)" :style="{ height: `${offsetAt(start + index + 1) - offsetAt(start + index)}px` }">
				<slot :item="item" :index="start + index" />
			</div>
		</div>
	</div>
</template>

<style scoped>
.rc-vlist {
	position: relative;
	min-width: 0;
}

.rc-vlist__window {
	position: absolute;
	inset: 0 0 auto;
	will-change: transform;
}
</style>

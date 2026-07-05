<script setup lang="ts">
import type { ComputedRef, Ref } from "vue";
import type { BatchItem } from "~/types/batch";

type ProgressState = { percent: ComputedRef<number>; message: Ref<string> };

const props = withDefaults(
  defineProps<{
    title: string;
    items: BatchItem[];
    selectedIds: Set<string>;
    draggable?: boolean;
    selectable?: boolean;
    retryable?: boolean;
    progressSlots?: ProgressState[];
  }>(),
  { draggable: false, selectable: false, retryable: false, progressSlots: () => [] },
);

const emit = defineEmits<{
  toggleSelect: [id: string];
  remove: [id: string];
  retry: [id: string];
  reorder: [orderedIds: string[]];
}>();

const now = ref(Date.now());
let timer: ReturnType<typeof setInterval> | undefined;
onMounted(() => {
  timer = setInterval(() => (now.value = Date.now()), 1000);
});
onUnmounted(() => {
  if (timer) clearInterval(timer);
});

function formatElapsed(ms: number): string {
  const totalSeconds = Math.max(0, Math.round(ms / 1000));
  const m = Math.floor(totalSeconds / 60);
  const s = totalSeconds % 60;
  return m > 0 ? `${m}m ${s}s` : `${s}s`;
}

function elapsed(item: BatchItem): string | null {
  if (item.status === "running" && item.startedAt) return formatElapsed(now.value - item.startedAt);
  if (item.elapsedMs != null) return formatElapsed(item.elapsedMs);
  return null;
}

function slotProgress(item: BatchItem): ProgressState | undefined {
  return item.slot != null ? props.progressSlots[item.slot] : undefined;
}

const draggingId = ref<string | null>(null);

function onDragStart(id: string) {
  draggingId.value = id;
}

function onDrop(targetId: string) {
  const dragged = draggingId.value;
  draggingId.value = null;
  if (!dragged || dragged === targetId) return;
  const pendingIds = props.items.filter((i) => i.status === "pending").map((i) => i.id);
  const from = pendingIds.indexOf(dragged);
  const to = pendingIds.indexOf(targetId);
  if (from < 0 || to < 0) return;
  pendingIds.splice(to, 0, ...pendingIds.splice(from, 1));
  emit("reorder", pendingIds);
}

function moveBy(id: string, delta: number) {
  const pendingIds = props.items.filter((i) => i.status === "pending").map((i) => i.id);
  const from = pendingIds.indexOf(id);
  const to = from + delta;
  if (from < 0 || to < 0 || to >= pendingIds.length) return;
  pendingIds.splice(to, 0, ...pendingIds.splice(from, 1));
  emit("reorder", pendingIds);
}
</script>

<template>
  <div v-if="items.length > 0" class="space-y-1">
    <div class="text-xs font-semibold uppercase tracking-wider text-zinc-500">
      {{ title }} ({{ items.length }})
    </div>
    <div class="space-y-1">
      <div
        v-for="item in items"
        :key="item.id"
        :draggable="draggable && item.status === 'pending'"
        class="group flex items-center gap-2 rounded-md px-2.5 py-1.5 text-sm"
        :class="{
          'bg-zinc-800/50': item.status === 'running',
          'cursor-grab': draggable && item.status === 'pending',
        }"
        @dragstart="onDragStart(item.id)"
        @dragover.prevent
        @drop="onDrop(item.id)"
      >
        <input
          v-if="selectable && item.status !== 'running'"
          type="checkbox"
          class="h-3.5 w-3.5 shrink-0 rounded border-zinc-600 bg-zinc-800 accent-sky-500"
          :checked="selectedIds.has(item.id)"
          :aria-label="`Select ${basename(item.input)}`"
          @change="emit('toggleSelect', item.id)"
        />

        <button
          v-if="draggable && item.status === 'pending'"
          type="button"
          class="shrink-0 cursor-grab text-zinc-600 transition hover:text-zinc-400 focus:text-zinc-300"
          :aria-label="`Reorder ${basename(item.input)}, arrow keys move`"
          @keydown.up.prevent="moveBy(item.id, -1)"
          @keydown.down.prevent="moveBy(item.id, 1)"
        >
          <svg class="h-3.5 w-3.5" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <circle cx="9" cy="6" r="1.5" /><circle cx="15" cy="6" r="1.5" />
            <circle cx="9" cy="12" r="1.5" /><circle cx="15" cy="12" r="1.5" />
            <circle cx="9" cy="18" r="1.5" /><circle cx="15" cy="18" r="1.5" />
          </svg>
        </button>

        <span class="flex h-4 w-4 shrink-0 items-center justify-center">
          <span v-if="item.status === 'pending'" class="h-2 w-2 rounded-full bg-zinc-600" />
          <svg
            v-else-if="item.status === 'running'"
            class="h-4 w-4 animate-spin text-sky-400"
            fill="none"
            viewBox="0 0 24 24"
            aria-hidden="true"
          >
            <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4" />
            <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
          </svg>
          <svg
            v-else-if="item.status === 'done'"
            class="h-4 w-4 text-emerald-400"
            fill="none"
            viewBox="0 0 24 24"
            stroke="currentColor"
            stroke-width="2"
            aria-hidden="true"
          >
            <path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7" />
          </svg>
          <svg
            v-else-if="item.status === 'error'"
            class="h-4 w-4 text-red-400"
            fill="none"
            viewBox="0 0 24 24"
            stroke="currentColor"
            stroke-width="2"
            aria-hidden="true"
          >
            <path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
          </svg>
          <svg
            v-else-if="item.status === 'cancelled'"
            class="h-4 w-4 text-amber-400"
            fill="none"
            viewBox="0 0 24 24"
            stroke="currentColor"
            stroke-width="2"
            aria-hidden="true"
          >
            <circle cx="12" cy="12" r="9" />
            <path stroke-linecap="round" d="M8 12h8" />
          </svg>
        </span>

        <div class="min-w-0 flex-1">
          <div class="truncate text-zinc-200" :title="item.input">
            {{ basename(item.input) }}
          </div>
          <div
            v-if="item.status === 'running' && slotProgress(item)"
            class="mt-1 h-1 w-full overflow-hidden rounded-full bg-zinc-700"
          >
            <div
              class="h-full rounded-full bg-gradient-to-r from-sky-500 to-sky-400 transition-all duration-300"
              :style="{ width: `${Math.min(slotProgress(item)!.percent.value, 100)}%` }"
            />
          </div>
          <div v-if="item.status === 'error' && item.error" class="mt-0.5 truncate text-xs text-red-400/80" :title="item.error">
            {{ item.error }}
          </div>
        </div>

        <span v-if="elapsed(item)" class="shrink-0 font-mono text-xs text-zinc-500">{{ elapsed(item) }}</span>

        <button
          v-if="retryable && (item.status === 'error' || item.status === 'cancelled')"
          class="shrink-0 text-zinc-400 opacity-0 transition hover:text-sky-300 group-hover:opacity-100"
          :aria-label="`Retry ${basename(item.input)}`"
          @click="emit('retry', item.id)"
        >
          <svg class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2" aria-hidden="true">
            <path
              stroke-linecap="round"
              stroke-linejoin="round"
              d="M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0l3.181 3.183a8.25 8.25 0 0013.803-3.7M4.031 9.865a8.25 8.25 0 0113.803-3.7l3.181 3.182m0-4.991v4.99"
            />
          </svg>
        </button>

        <button
          v-if="item.status !== 'running'"
          class="shrink-0 text-zinc-400 opacity-0 transition hover:text-zinc-300 group-hover:opacity-100"
          :aria-label="`Remove ${basename(item.input)} from queue`"
          @click="emit('remove', item.id)"
        >
          <svg class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2" aria-hidden="true">
            <path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
          </svg>
        </button>
      </div>
    </div>
  </div>
</template>

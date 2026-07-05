<script setup lang="ts">
import type { ComputedRef, Ref } from "vue";
import type { BatchItem } from "~/types/batch";

type ProgressState = { percent: ComputedRef<number>; message: Ref<string> };

// Concrete Ref/ComputedRef types so ProgressState from useProgress
// passes through without widening every consumer.
const props = withDefaults(
  defineProps<{
    items: BatchItem[];
    running: boolean;
    progressSlots?: ProgressState[];
    // Hide the concurrency control, drag-reorder, selection, and retry-failed
    // UI for pages whose queue is a single N-to-1 bundle op rather than a set
    // of independently retryable jobs.
    queueActions?: boolean;
  }>(),
  { progressSlots: () => [], queueActions: true },
);

const emit = defineEmits<{
  remove: [id: string];
  clear: [];
  reorder: [orderedIds: string[]];
  removeSelected: [ids: string[]];
  retryFailed: [];
}>();

const { concurrency, maxConcurrency } = useJobConcurrency();

const activeItems = computed(() => props.items.filter((i) => i.status === "pending" || i.status === "running"));
const completedItems = computed(() => props.items.filter((i) => i.status === "done"));
const failedItems = computed(() => props.items.filter((i) => i.status === "error" || i.status === "cancelled"));

const settledCount = computed(
  () => props.items.filter((i) => i.status === "done" || i.status === "error" || i.status === "cancelled").length,
);

// Settled items count as 1, running items contribute their slot's fraction,
// so the bar advances smoothly instead of jumping per completed file.
const overallPercent = computed(() => {
  const total = props.items.length;
  if (total === 0) return 0;
  let done = settledCount.value;
  for (const item of props.items) {
    if (item.status === "running" && item.slot !== undefined) {
      done += (props.progressSlots[item.slot]?.percent.value ?? 0) / 100;
    }
  }
  return Math.min(100, Math.round((done / total) * 100));
});

const selectedIds = ref(new Set<string>());

function toggleSelect(id: string) {
  const next = new Set(selectedIds.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  selectedIds.value = next;
}

// Requeueing happens here rather than in the batch runner so single-item
// retry and bulk retry share one path: mark pending, then let the page's
// retryFailed handler re-run the queue.
function requeue(item: BatchItem) {
  item.status = "pending";
  item.error = undefined;
  item.result = undefined;
  item.elapsedMs = undefined;
}

function retryFailed() {
  for (const item of props.items) {
    if (item.status === "error" || item.status === "cancelled") requeue(item);
  }
  emit("retryFailed");
}

function retryItem(id: string) {
  const item = props.items.find((i) => i.id === id);
  if (item && (item.status === "error" || item.status === "cancelled")) {
    requeue(item);
    emit("retryFailed");
  }
}

function removeItem(id: string) {
  selectedIds.value.delete(id);
  emit("remove", id);
}

function removeSelected() {
  emit("removeSelected", [...selectedIds.value]);
  selectedIds.value = new Set();
}

watch(() => props.items, (items) => {
  const ids = new Set(items.map((i) => i.id));
  for (const id of selectedIds.value) {
    if (!ids.has(id)) selectedIds.value.delete(id);
  }
});
</script>

<template>
  <div class="space-y-2">
    <div class="flex items-center justify-between">
      <span class="text-sm font-medium text-zinc-300">
        Files ({{ items.length }})
      </span>
      <div class="flex items-center gap-3">
        <button
          v-if="queueActions && !running && selectedIds.size > 0"
          class="text-xs text-zinc-500 transition hover:text-zinc-300"
          @click="removeSelected"
        >
          Remove selected ({{ selectedIds.size }})
        </button>
        <button
          v-if="queueActions && !running && failedItems.length > 0"
          class="text-xs text-amber-400 transition hover:text-amber-300"
          @click="retryFailed"
        >
          Retry failed ({{ failedItems.length }})
        </button>
        <button
          v-if="!running && items.length > 0"
          class="text-xs text-zinc-500 transition hover:text-zinc-300"
          @click="emit('clear')"
        >
          Clear all
        </button>
      </div>
    </div>

    <ProgressBar
      v-if="running && items.length > 1"
      :percent="overallPercent"
      :message="`Overall: ${settledCount} of ${items.length} processed`"
      :running="running"
    />

    <div class="max-h-60 space-y-2 overflow-y-auto rounded-lg border border-zinc-700/50 bg-zinc-900/50 p-2 xl:max-h-80">
      <QueueSection
        title="Active"
        :items="activeItems"
        :selected-ids="selectedIds"
        :draggable="queueActions"
        :selectable="queueActions"
        :progress-slots="progressSlots"
        @toggle-select="toggleSelect"
        @remove="removeItem"
        @reorder="(ids) => emit('reorder', ids)"
      />
      <QueueSection
        title="Completed"
        :items="completedItems"
        :selected-ids="selectedIds"
        :selectable="queueActions"
        @toggle-select="toggleSelect"
        @remove="removeItem"
      />
      <QueueSection
        title="Failed"
        :items="failedItems"
        :selected-ids="selectedIds"
        :selectable="queueActions"
        :retryable="queueActions && !running"
        @toggle-select="toggleSelect"
        @remove="removeItem"
        @retry="retryItem"
      />
    </div>

    <div class="flex items-center justify-between gap-3">
      <div v-if="items.length > 0" role="status" aria-live="polite" class="text-xs text-zinc-500">
        <template v-if="running">
          {{ activeItems.length }} remaining, {{ completedItems.length }} done<template v-if="failedItems.length > 0">, {{ failedItems.length }} failed</template>
        </template>
        <template v-else-if="completedItems.length > 0 || failedItems.length > 0">
          {{ completedItems.length }} of {{ items.length }} complete<template v-if="failedItems.length > 0">, {{ failedItems.length }} failed</template>
        </template>
        <template v-else>
          {{ items.length }} file{{ items.length === 1 ? '' : 's' }} queued
        </template>
      </div>

      <label v-if="queueActions" class="flex shrink-0 items-center gap-1.5 text-xs text-zinc-500">
        Concurrent jobs
        <input
          v-model.number="concurrency"
          type="number"
          min="1"
          :max="maxConcurrency"
          class="w-12 rounded-md border border-zinc-700 bg-zinc-800/50 px-1.5 py-0.5 text-center text-xs text-zinc-200"
        />
      </label>
    </div>
  </div>
</template>

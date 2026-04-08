<script setup lang="ts">
import type { BatchItem } from "~/types/batch";

const props = defineProps<{
  items: BatchItem[];
  currentIndex: number;
  running: boolean;
  progress?: { percent: ReturnType<typeof computed<number>>; message: ReturnType<typeof ref<string>> };
}>();

const emit = defineEmits<{
  remove: [id: string];
  clear: [];
}>();

function fileName(path: string) {
  const parts = path.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || path;
}

const doneCount = computed(() => props.items.filter((i) => i.status === "done").length);
const errorCount = computed(() => props.items.filter((i) => i.status === "error").length);

const listRef = ref<HTMLElement | null>(null);

watch(() => props.currentIndex, (idx) => {
  if (idx < 0 || !listRef.value) return;
  const children = listRef.value.children;
  if (children[idx]) {
    children[idx].scrollIntoView({ block: "nearest", behavior: "smooth" });
  }
});
</script>

<template>
  <div class="space-y-2">
    <div class="flex items-center justify-between">
      <span class="text-sm font-medium text-zinc-300">
        Files ({{ items.length }})
      </span>
      <button
        v-if="!running && items.length > 0"
        class="text-xs text-zinc-500 transition hover:text-zinc-300"
        @click="emit('clear')"
      >
        Clear all
      </button>
    </div>

    <div ref="listRef" class="max-h-60 space-y-1 overflow-y-auto rounded-lg border border-zinc-700/50 bg-zinc-900/50 p-2 xl:max-h-80">
      <div
        v-for="(item, index) in items"
        :key="item.id"
        class="group flex items-center gap-2 rounded-md px-2.5 py-1.5 text-sm"
        :class="{
          'bg-zinc-800/50': item.status === 'running',
        }"
      >
        <!-- Status icon -->
        <span class="flex h-4 w-4 shrink-0 items-center justify-center">
          <!-- Pending -->
          <span
            v-if="item.status === 'pending'"
            class="h-2 w-2 rounded-full bg-zinc-600"
          />
          <!-- Running spinner -->
          <svg
            v-else-if="item.status === 'running'"
            class="h-4 w-4 animate-spin text-sky-400"
            fill="none"
            viewBox="0 0 24 24"
          >
            <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4" />
            <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
          </svg>
          <!-- Done check -->
          <svg
            v-else-if="item.status === 'done'"
            class="h-4 w-4 text-emerald-400"
            fill="none"
            viewBox="0 0 24 24"
            stroke="currentColor"
            stroke-width="2"
          >
            <path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7" />
          </svg>
          <!-- Error x -->
          <svg
            v-else-if="item.status === 'error'"
            class="h-4 w-4 text-red-400"
            fill="none"
            viewBox="0 0 24 24"
            stroke="currentColor"
            stroke-width="2"
          >
            <path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
          </svg>
        </span>

        <!-- File info -->
        <div class="min-w-0 flex-1">
          <div class="truncate text-zinc-200" :title="item.input">
            {{ fileName(item.input) }}
          </div>
          <!-- Inline progress bar for running item -->
          <div
            v-if="item.status === 'running' && progress"
            class="mt-1 h-1 w-full overflow-hidden rounded-full bg-zinc-700"
          >
            <div
              class="h-full rounded-full bg-gradient-to-r from-sky-500 to-sky-400 transition-all duration-300"
              :style="{ width: `${Math.min(progress.percent.value, 100)}%` }"
            />
          </div>
          <!-- Error message -->
          <div v-if="item.status === 'error' && item.error" class="mt-0.5 truncate text-xs text-red-400/80" :title="item.error">
            {{ item.error }}
          </div>
        </div>

        <!-- Remove button (only for pending items when not running) -->
        <button
          v-if="item.status === 'pending' && !running"
          class="shrink-0 text-zinc-600 opacity-0 transition hover:text-zinc-300 group-hover:opacity-100"
          @click="emit('remove', item.id)"
        >
          <svg class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
            <path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
          </svg>
        </button>
      </div>
    </div>

    <!-- Summary footer -->
    <div v-if="items.length > 0" class="text-xs text-zinc-500">
      <template v-if="running">
        Processing {{ currentIndex + 1 }} of {{ items.length }}...
      </template>
      <template v-else-if="doneCount > 0 || errorCount > 0">
        {{ doneCount }} of {{ items.length }} complete<template v-if="errorCount > 0">, {{ errorCount }} failed</template>
      </template>
      <template v-else>
        {{ items.length }} file{{ items.length === 1 ? '' : 's' }} queued
      </template>
    </div>
  </div>
</template>

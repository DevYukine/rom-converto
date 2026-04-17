<script setup lang="ts">
import { open, save } from "@tauri-apps/plugin-dialog";

const props = defineProps<{
  label: string;
  modelValue: string;
  directory?: boolean;
  saveDialog?: boolean;
  primary?: boolean;
  multiple?: boolean;
  filters?: { name: string; extensions: string[] }[];
}>();

const emit = defineEmits<{
  "update:modelValue": [value: string];
  "update:files": [paths: string[]];
}>();

const dropZoneRef = ref<HTMLElement | null>(null);
let zoneId: string | null = null;

onMounted(() => {
  if (dropZoneRef.value) {
    zoneId = registerDropZone(
      dropZoneRef.value,
      (paths) => {
        const first = paths[0];
        if (first === undefined) return;
        if (props.multiple && paths.length > 1) {
          emit("update:files", paths);
        } else {
          emit("update:modelValue", first);
        }
      },
      props.primary ? 0 : 100,
    );
  }
});

onUnmounted(() => {
  if (zoneId) unregisterDropZone(zoneId);
});

function fileName(path: string) {
  const parts = path.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || path;
}

function clear() {
  emit("update:modelValue", "");
}

async function browse() {
  if (props.saveDialog) {
    const result = await save({ filters: props.filters });
    if (result) {
      emit("update:modelValue", result);
    }
  } else {
    const result = await open({
      directory: props.directory ?? false,
      multiple: props.multiple ?? false,
      filters: props.filters,
    });
    if (result) {
      if (Array.isArray(result) && result.length > 1) {
        emit("update:files", result);
      } else {
        const single = Array.isArray(result) ? result[0] : result;
        emit("update:modelValue", single);
      }
    }
  }
}
</script>

<template>
  <div class="space-y-1.5">
    <label class="block text-sm font-medium text-zinc-300">{{ label }}</label>

    <!-- Empty state: drop target -->
    <div
      v-if="!modelValue"
      ref="dropZoneRef"
      class="drop-zone flex cursor-pointer flex-col items-center justify-center gap-2 rounded-lg border-2 border-dashed border-zinc-700 bg-zinc-800/30 px-4 py-6 transition hover:border-zinc-500 hover:bg-zinc-800/50 xl:py-8"
      @click="browse"
    >
      <!-- Upload icon -->
      <svg class="h-8 w-8 text-zinc-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
        <path stroke-linecap="round" stroke-linejoin="round" d="M3 16.5v2.25A2.25 2.25 0 005.25 21h13.5A2.25 2.25 0 0021 18.75V16.5m-13.5-9L12 3m0 0l4.5 4.5M12 3v13.5" />
      </svg>
      <span class="text-sm text-zinc-400">
        {{ directory ? "Drop a folder or click to browse" : multiple ? "Drop files or click to browse" : "Drop a file or click to browse" }}
      </span>
    </div>

    <!-- Populated state: show file info -->
    <div
      v-else
      ref="dropZoneRef"
      class="drop-zone flex flex-col items-center justify-center gap-2 rounded-lg border border-zinc-700 bg-zinc-800/30 px-4 py-6 transition xl:py-8"
    >
      <!-- File icon -->
      <svg class="h-8 w-8 text-sky-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
        <path stroke-linecap="round" stroke-linejoin="round" d="M19.5 14.25v-2.625a3.375 3.375 0 00-3.375-3.375h-1.5A1.125 1.125 0 0113.5 7.125v-1.5a3.375 3.375 0 00-3.375-3.375H8.25m2.25 0H5.625c-.621 0-1.125.504-1.125 1.125v17.25c0 .621.504 1.125 1.125 1.125h12.75c.621 0 1.125-.504 1.125-1.125V11.25a9 9 0 00-9-9z" />
      </svg>
      <div class="truncate text-sm font-medium text-zinc-200">{{ fileName(modelValue) }}</div>
      <div class="max-w-full truncate text-xs text-zinc-500" :title="modelValue">{{ modelValue }}</div>
      <div class="mt-1 flex items-center gap-2">
        <button
          type="button"
          class="rounded-md bg-zinc-700/50 px-3 py-1 text-xs font-medium text-zinc-300 transition hover:bg-zinc-700 hover:text-zinc-100"
          @click="browse"
        >
          Change
        </button>
        <button
          v-if="!saveDialog"
          type="button"
          class="rounded-md bg-zinc-700/50 px-3 py-1 text-xs font-medium text-zinc-400 transition hover:bg-zinc-700 hover:text-zinc-200"
          @click="clear"
        >
          Clear
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.drop-zone.drop-hover {
  border-color: rgb(14 165 233); /* sky-500 */
  background-color: rgb(14 165 233 / 0.08);
  box-shadow: 0 0 0 1px rgb(14 165 233 / 0.3);
}
</style>

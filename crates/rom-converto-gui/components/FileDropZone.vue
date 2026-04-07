<script setup lang="ts">
import { open, save } from "@tauri-apps/plugin-dialog";

const props = defineProps<{
  label: string;
  modelValue: string;
  directory?: boolean;
  saveDialog?: boolean;
  primary?: boolean;
  filters?: { name: string; extensions: string[] }[];
}>();

const emit = defineEmits<{
  "update:modelValue": [value: string];
}>();

const dropZoneRef = ref<HTMLElement | null>(null);
let zoneId: string | null = null;

onMounted(() => {
  if (dropZoneRef.value) {
    zoneId = registerDropZone(
      dropZoneRef.value,
      (paths) => {
        if (paths.length > 0) {
          emit("update:modelValue", paths[0]);
        }
      },
      props.primary ? 0 : 100,
    );
  }
});

onUnmounted(() => {
  if (zoneId) unregisterDropZone(zoneId);
});

async function browse() {
  if (props.saveDialog) {
    const result = await save({ filters: props.filters });
    if (result) {
      emit("update:modelValue", result);
    }
  } else {
    const result = await open({
      directory: props.directory ?? false,
      multiple: false,
      filters: props.filters,
    });
    if (result) {
      emit("update:modelValue", result as string);
    }
  }
}
</script>

<template>
  <div class="space-y-1">
    <label class="block text-sm font-medium text-zinc-300">{{ label }}</label>
    <div
      ref="dropZoneRef"
      class="drop-zone flex items-center gap-2 rounded-lg border border-zinc-700 bg-zinc-800/50 p-3 transition hover:border-zinc-500"
    >
      <input
        type="text"
        :value="modelValue"
        readonly
        :placeholder="
          directory
            ? 'Drop a folder or click Browse...'
            : 'Drop a file or click Browse...'
        "
        class="flex-1 bg-transparent text-sm text-zinc-200 outline-none placeholder:text-zinc-500"
      />
      <button
        type="button"
        class="rounded-md bg-zinc-700 px-3 py-1.5 text-xs font-medium text-zinc-200 transition hover:bg-zinc-600"
        @click="browse"
      >
        Browse
      </button>
    </div>
  </div>
</template>

<style scoped>
.drop-zone.drop-hover {
  border-color: rgb(14 165 233); /* sky-500 */
  background-color: rgb(14 165 233 / 0.1);
}
</style>

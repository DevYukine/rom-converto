<script setup lang="ts">
import { useConfigStore } from "~/stores/config";
import type { PresetFormat } from "~/types/config";

const props = defineProps<{ format: PresetFormat; onSave: (name: string) => Promise<void> }>();

const configStore = useConfigStore();
if (!configStore.loaded) configStore.loadConfig();

const newName = ref("");
const saving = ref(false);
const presetNames = computed(() => Object.keys(configStore.presets).sort());

function selectPreset(name: string) {
  configStore.applyPreset(name);
}

async function saveCurrent() {
  const name = newName.value.trim();
  if (!name || saving.value) return;
  saving.value = true;
  try {
    await props.onSave(name);
    newName.value = "";
  } catch {
    // surfaced via configStore.error below
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <div class="flex flex-wrap items-end gap-4 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
    <div v-if="configStore.error" class="w-full rounded-md border border-red-900/50 bg-red-950/30 px-3 py-2 text-sm text-red-300">
      {{ configStore.error }}
    </div>
    <label class="flex flex-col gap-1.5">
      <span class="text-sm font-medium text-zinc-200">Preset</span>
      <select
        :value="configStore.activePreset ?? ''"
        class="rounded-md border border-zinc-700 bg-zinc-800/50 px-3 py-1.5 text-sm text-zinc-200"
        @change="selectPreset(($event.target as HTMLSelectElement).value)"
      >
        <option value="">None</option>
        <option v-for="name in presetNames" :key="name" :value="name">{{ name }}</option>
      </select>
    </label>
    <label class="flex min-w-[14rem] flex-1 flex-col gap-1.5">
      <span class="text-sm font-medium text-zinc-200">Save current options as</span>
      <div class="flex gap-2">
        <input
          v-model="newName"
          type="text"
          placeholder="preset name"
          class="flex-1 rounded-md border border-zinc-700 bg-zinc-800/50 px-3 py-1.5 text-sm text-zinc-200"
          @keyup.enter="saveCurrent"
        />
        <button
          type="button"
          class="shrink-0 rounded-md bg-sky-600 px-3 py-1.5 text-sm font-medium text-white transition hover:bg-sky-500 disabled:cursor-not-allowed disabled:opacity-40"
          :disabled="!newName.trim() || saving"
          @click="saveCurrent"
        >
          Save
        </button>
      </div>
    </label>
  </div>
</template>

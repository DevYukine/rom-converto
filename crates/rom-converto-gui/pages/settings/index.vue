<script setup lang="ts">
import { useConfigStore } from "~/stores/config";
import type { Preset, PresetFormat } from "~/types/config";

const store = useConfigStore();
if (!store.loaded) store.loadConfig();

const presetNames = computed(() => Object.keys(store.presets).sort());

const FORMAT_LABELS: Record<PresetFormat, string> = {
  dol: "GameCube (dol)",
  rvl: "Wii (rvl)",
  nx: "Switch (nx)",
  chd: "CHD",
  cso: "CSO/ZSO",
  wup: "Wii U (wup)",
  dat: "DAT",
};

function setFormats(preset: Preset | undefined): string {
  if (!preset) return "empty";
  const formats = (Object.keys(preset) as PresetFormat[]).filter((k) => preset[k]);
  return formats.map((f) => FORMAT_LABELS[f] ?? f).join(", ") || "empty";
}

// Two-click confirm instead of a modal: the first click arms the button,
// which disarms itself after a moment if the user hesitates.
const confirmingDelete = ref<string | null>(null);
let confirmTimer: ReturnType<typeof setTimeout> | undefined;

async function deletePreset(name: string) {
  if (confirmingDelete.value !== name) {
    confirmingDelete.value = name;
    clearTimeout(confirmTimer);
    confirmTimer = setTimeout(() => (confirmingDelete.value = null), 3000);
    return;
  }
  clearTimeout(confirmTimer);
  confirmingDelete.value = null;
  try {
    await store.deletePreset(name);
  } catch {
    // surfaced via store.error above
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Settings"
      description="Presets are read from and written to the same rom-converto.toml the CLI uses, so a profile saved here is reproducible from the command line."
      :has-error="!!store.error"
    />

    <OperationCard>
      <div class="space-y-6">
        <div>
          <span class="text-sm font-medium text-zinc-200">Config file</span>
          <p class="mt-1 break-all rounded-md border border-zinc-800/50 bg-zinc-800/20 px-3 py-2 font-mono text-xs text-zinc-400">
            {{ store.configPath ?? "none found; a preset save creates one at the per-user config path" }}
          </p>
          <p class="mt-2 text-xs text-zinc-500">
            Saving a preset only rewrites its own <code>[presets.&lt;name&gt;]</code> table. Comments and
            every other table or key already in the file are left untouched. Deleting a preset removes only
            that table.
          </p>
        </div>

        <div v-if="store.error" class="rounded-md border border-red-900/50 bg-red-950/30 px-3 py-2 text-sm text-red-300">
          {{ store.error }}
        </div>

        <div>
          <span class="text-sm font-medium text-zinc-200">Presets</span>
          <p v-if="presetNames.length === 0" class="mt-2 text-sm text-zinc-500">
            No presets yet. Use a compress page's "Save current options as" control to create one.
          </p>
          <ul v-else class="mt-2 divide-y divide-zinc-800/50 rounded-lg border border-zinc-800/50">
            <li
              v-for="name in presetNames"
              :key="name"
              class="flex items-center justify-between gap-4 px-4 py-3"
            >
              <div class="min-w-0">
                <span class="font-medium text-zinc-200">{{ name }}</span>
                <p class="truncate text-xs text-zinc-500">{{ setFormats(store.presets[name]) }}</p>
              </div>
              <div class="flex shrink-0 items-center gap-3">
                <button
                  type="button"
                  class="rounded-md bg-zinc-700/60 px-3 py-1.5 text-xs font-medium text-zinc-200 transition hover:bg-zinc-700"
                  :class="{ 'ring-1 ring-sky-500 text-sky-300': store.activePreset === name }"
                  @click="store.applyPreset(name)"
                >
                  {{ store.activePreset === name ? "Active" : "Make active" }}
                </button>
                <button
                  type="button"
                  class="rounded-md px-3 py-1.5 text-xs font-medium transition"
                  :class="confirmingDelete === name
                    ? 'bg-red-800/80 text-red-100 hover:bg-red-800'
                    : 'bg-red-900/40 text-red-300 hover:bg-red-900/60'"
                  @click="deletePreset(name)"
                >
                  {{ confirmingDelete === name ? "Confirm delete" : "Delete" }}
                </button>
              </div>
            </li>
          </ul>
        </div>

        <button
          v-if="store.activePreset"
          type="button"
          class="text-xs text-zinc-400 underline hover:text-zinc-300"
          @click="store.applyPreset(null)"
        >
          Clear active preset
        </button>

        <div class="border-t border-zinc-800 pt-4">
          <h3 class="text-sm font-medium text-zinc-300">DAT verification</h3>
          <dl class="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-sm lg:max-w-md">
            <dt class="text-zinc-500">Checksum floor</dt>
            <dd class="text-zinc-200">{{ store.dat?.input_checksum_min ?? "crc32 (default)" }}</dd>
            <dt class="text-zinc-500">Checksum ceiling</dt>
            <dd class="text-zinc-200">{{ store.dat?.input_checksum_max ?? "sha256 (default)" }}</dd>
          </dl>
          <p class="mt-2 text-xs text-zinc-500">
            Verify computes the floor tier first and escalates only when it does not resolve a
            match. Set <code>input_checksum_min</code> and <code>input_checksum_max</code> under
            <code>[dat]</code> in the config file to change the policy for the CLI and GUI alike.
          </p>
        </div>
      </div>
    </OperationCard>
  </div>
</template>

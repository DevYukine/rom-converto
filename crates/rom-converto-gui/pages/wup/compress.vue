<script setup lang="ts">
import { open } from "@tauri-apps/plugin-dialog";
import { storeToRefs } from "pinia";
import { isDiscInput, useWupCompressStore } from "~/stores/wup-compress";

const store = useWupCompressStore();
const { queue, output, level, keys, result, error, loading } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("wup-compress");

// One drop zone for both folders and disc images. Drag-drop takes
// whatever the OS hands us. The two browse buttons pick folder vs
// file mode because a native picker can only do one.
const dropZoneRef = ref<HTMLElement | null>(null);
let zoneId: string | null = null;

function addPaths(paths: string[]) {
  for (const p of paths) {
    if (p) store.addToQueue(p);
  }
  // Fill in a default output on first add. Never overwrite a path
  // the user has already picked.
  if (!output.value && queue.value.length > 0) {
    const first = queue.value[0];
    if (first) output.value = deriveWuaPath(first.input);
  }
}

onMounted(() => {
  if (dropZoneRef.value) {
    zoneId = registerDropZone(dropZoneRef.value, addPaths, 0);
  }
});

onUnmounted(() => {
  if (zoneId) unregisterDropZone(zoneId);
});

async function browseDirectories() {
  const result = await open({ directory: true, multiple: true });
  if (!result) return;
  addPaths(Array.isArray(result) ? result : [result]);
}

async function browseDiscImages() {
  const result = await open({
    directory: false,
    multiple: true,
    filters: [{ name: "Wii U disc images", extensions: ["wud", "wux"] }],
  });
  if (!result) return;
  addPaths(Array.isArray(result) ? result : [result]);
}

function handleKeyChange(id: string, path: string) {
  store.setKey(id, path);
}

// Discs without a key show a warning, not a hard block: compression
// falls back to a sibling `<disc>.key` / `game.key` on disk.
const discsMissingKeys = computed(() =>
  queue.value
    .filter((i) => isDiscInput(i.input))
    .filter((i) => !keys.value[i.id]),
);

const canCompress = computed(() => queue.value.length > 0 && !!output.value);

async function execute() {
  progress.reset();
  // N-to-1 bundle: every queued title shares the run's terminal
  // state, so mark them all running up front.
  for (const item of queue.value) {
    if (item.status === "pending" || item.status === "done" || item.status === "error") {
      item.status = "running";
    }
  }
  await run("cmd_wup_compress", {
    inputs: queue.value.map((i) => i.input),
    output: output.value,
    level: level.value,
    keys: store.collectKeys(),
  });
  const terminal: "done" | "error" = error.value ? "error" : "done";
  for (const item of queue.value) {
    if (item.status === "running") item.status = terminal;
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Compress to WUA"
      description="Bundle Wii U titles into a single Cemu .wua archive. Inputs can be loadiine or NUS directories or .wud / .wux disc images; any mix works. Disc images need a per-disc master key."
      :loading="loading"
      :has-result="!!result"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <BatchFileList
          v-if="queue.length > 0"
          :items="queue"
          :current-index="-1"
          :running="loading"
          @remove="store.removeFromQueue"
          @clear="store.clearQueue"
        />

        <!-- Per-disc master key pickers. Only for .wud / .wux items. -->
        <div
          v-for="item in queue.filter((i) => isDiscInput(i.input))"
          :key="`key-${item.id}`"
          class="rounded-lg border border-amber-700/40 bg-amber-900/10 px-4 py-3"
        >
          <div class="text-sm font-medium text-amber-200">
            Disc master key for {{ item.input.split(/[\\/]/).pop() }}
          </div>
          <div class="text-xs text-zinc-400 mt-0.5">
            16 raw bytes or 32 hex chars. Leave empty to auto-discover a
            sibling &lt;disc&gt;.key or game.key file.
          </div>
          <div class="mt-2">
            <FileDropZone
              :model-value="keys[item.id] ?? ''"
              label="Key file"
              :filters="[{ name: 'Disc key', extensions: ['key', 'bin', 'txt'] }]"
              @update:model-value="(p: string) => handleKeyChange(item.id, p)"
            />
          </div>
        </div>

        <!-- One drop area, two browse buttons (folder vs disc image). -->
        <div class="space-y-1.5">
          <label class="block text-sm font-medium text-zinc-300">
            {{ queue.length > 0 ? "Add more inputs" : "Inputs" }}
          </label>
          <div
            ref="dropZoneRef"
            class="drop-zone flex cursor-default flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed border-zinc-700 bg-zinc-800/30 px-4 py-6 transition xl:py-8"
          >
            <svg class="h-8 w-8 text-zinc-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
              <path stroke-linecap="round" stroke-linejoin="round" d="M3 16.5v2.25A2.25 2.25 0 005.25 21h13.5A2.25 2.25 0 0021 18.75V16.5m-13.5-9L12 3m0 0l4.5 4.5M12 3v13.5" />
            </svg>
            <span class="text-center text-sm text-zinc-400">
              Drop title directories (loadiine or NUS) or disc
              images (.wud / .wux). Mix and match freely.
            </span>
            <div class="flex flex-wrap items-center justify-center gap-2 pt-1">
              <button
                type="button"
                class="rounded-md bg-zinc-700/60 px-3 py-1.5 text-xs font-medium text-zinc-200 transition hover:bg-zinc-700"
                @click="browseDirectories"
              >
                Browse folder
              </button>
              <button
                type="button"
                class="rounded-md bg-zinc-700/60 px-3 py-1.5 text-xs font-medium text-zinc-200 transition hover:bg-zinc-700"
                @click="browseDiscImages"
              >
                Browse disc image
              </button>
            </div>
          </div>
        </div>

        <FileDropZone
          v-model="output"
          label="Output .wua path"
          :save-dialog="true"
          :filters="[{ name: 'Wii U Archive', extensions: ['wua'] }]"
        />

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <label class="flex flex-col gap-1.5">
            <span class="text-sm font-medium text-zinc-200">Zstd level</span>
            <span class="text-xs text-zinc-400">
              0 uses the Cemu default (6). 1 is fastest, 22 is max ratio.
              Higher levels produce smaller files at the cost of compression time.
            </span>
            <div class="flex items-center gap-3 pt-1">
              <input
                v-model.number="level"
                type="range"
                min="0"
                max="22"
                step="1"
                class="flex-1 accent-sky-500"
              />
              <span class="w-16 shrink-0 text-right font-mono text-sm text-zinc-200">
                {{ level === 0 ? "default" : level }}
              </span>
            </div>
          </label>
        </div>

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton
          :loading="loading"
          :disabled="!canCompress"
          @click="execute"
        >
          {{ queue.length <= 1 ? "Compress" : `Compress ${queue.length} titles` }}
        </RunButton>

        <div
          v-if="discsMissingKeys.length > 0"
          class="text-xs text-amber-300/80"
        >
          {{ discsMissingKeys.length }} disc input{{ discsMissingKeys.length === 1 ? "" : "s" }}
          without an explicit key; the backend will try sibling &lt;disc&gt;.key / game.key files.
        </div>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="result" :error="error" />
    </div>
  </div>
</template>

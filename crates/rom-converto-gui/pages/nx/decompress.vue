<script setup lang="ts">
import { open } from "@tauri-apps/plugin-dialog";
import { storeToRefs } from "pinia";
import { useNxDecompressStore } from "~/stores/nx-decompress";

const store = useNxDecompressStore();
const { queue, output, keys, result, error, loading } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const progress = useProgress("nx-decompress");

const commandLine = ref("");

function decompressArgs(item: { input: string; output: string }) {
  return { input: item.input, output: item.output, keys: keys.value || null };
}

const batch = useBatchOperation("nx-decompress", "cmd_nx_decompress", decompressArgs);

const dropZoneRef = ref<HTMLElement | null>(null);
let zoneId: string | null = null;

function addPaths(paths: string[]) {
  for (const p of paths) {
    if (p) store.addToQueue(p);
  }
  if (!output.value && queue.value.length > 0) {
    const first = queue.value[0];
    if (first) output.value = resolve(deriveNspPath(first.input));
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

watch(outputDir, () => {
  if (output.value && queue.value.length === 1) {
    const first = queue.value[0];
    if (first) output.value = resolve(deriveNspPath(first.input));
  }
  for (const item of queue.value) {
    if (item.status === "pending") {
      item.output = resolve(deriveNspPath(item.input));
    }
  }
});

async function browseInputs() {
  const result = await open({
    directory: false,
    multiple: true,
    filters: [{ name: "Switch compressed", extensions: ["nsz", "xcz"] }],
  });
  if (!result) return;
  addPaths(Array.isArray(result) ? result : [result]);
}

const canDecompress = computed(() => queue.value.length > 0 && !!output.value);

async function execute() {
  progress.reset();
  for (const item of queue.value) {
    item.output =
      queue.value.length === 1
        ? output.value
        : resolve(deriveNspPath(item.input));
  }
  const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
  commandLine.value = rep ? buildCliCommand("cmd_nx_decompress", decompressArgs(rep)) : "";
  await batch.start(queue, result);
}
</script>

<template>
  <div>
    <PageHeader
      title="Decompress to NSP/XCI"
      description="Decompress NSZ to NSP or XCZ to XCI. Output is byte-identical to the original installable container. Drop multiple files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="!!result"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <BatchFileList
          v-if="queue.length > 0"
          :items="queue"
          :current-index="batch.currentIndex.value"
          :running="batch.running.value"
          :progress="batch.progress"
          @remove="store.removeFromQueue"
          @clear="store.clearQueue"
        />

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
              Drop NSZ or XCZ files. Multiple inputs queue sequentially.
            </span>
            <button
              type="button"
              class="rounded-md bg-zinc-700/60 px-3 py-1.5 text-xs font-medium text-zinc-200 transition hover:bg-zinc-700"
              @click="browseInputs"
            >
              Browse files
            </button>
          </div>
        </div>

        <FileDropZone
          v-if="queue.length <= 1"
          v-model="output"
          label="Output file (auto-filled)"
          :save-dialog="true"
          :filters="[{ name: 'Switch container', extensions: ['nsp', 'xci'] }]"
        />
        <div
          v-else
          class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3 text-xs text-zinc-400"
        >
          Multi-file batch: each output is derived from its input (.nsz -> .nsp,
          .xcz -> .xci) next to the source file.
        </div>

        <FileDropZone
          v-model="keys"
          label="prod.keys (optional, falls back to ~/.switch/prod.keys)"
          :filters="[{ name: 'prod.keys', extensions: ['keys', 'txt'] }]"
        />

        <OutputDirField v-model="outputDir" />

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton
          :loading="loading || batch.running.value"
          :batch-current="batch.currentIndex.value"
          :batch-total="queue.length"
          :disabled="!canDecompress"
          @click="execute"
        >
          {{ queue.filter(i => i.status === 'pending').length > 1 ? `Decompress All (${queue.filter(i => i.status === 'pending').length})` : 'Decompress' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :error="error" />
    </div>
  </div>
</template>

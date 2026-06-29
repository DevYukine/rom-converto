<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { storeToRefs } from "pinia";
import { useChdExtractStore } from "~/stores/chd-extract";

const store = useChdExtractStore();
const { input, output, parent, result, error, loading, queue } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("chd-extract");

const isBatch = computed(() => queue.value.length > 0);
const commandLine = ref("");

function extractArgs(inputPath: string, outputPath: string) {
  return { input: inputPath, output: outputPath, parent: parent.value || null };
}

const batch = useBatchOperation("chd-extract", "cmd_chd_extract", (item) =>
  extractArgs(item.input, item.output),
);

// DVD-mode CHDs extract to a single .iso, CD-mode to .cue/.bin;
// the mode is only knowable from the file's metadata.
async function deriveOutput(path: string): Promise<string> {
  try {
    const raw = await invoke<string>("cmd_read_info", { input: path, keys: null });
    const parsed = JSON.parse(raw);
    if (parsed?.dvd) return deriveDiscIsoPath(path);
  } catch {
    // Fall through to the CD default.
  }
  return deriveCuePath(path);
}

watch([input, outputDir], async () => {
  if (input.value) output.value = resolve(await deriveOutput(input.value));
});

watch(outputDir, () => {
  for (const it of queue.value) {
    if (it.status === "pending") it.output = withOutputDir(it.output, outputDir.value);
  }
});

async function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, resolve(await deriveOutput(p)));
  }
}

async function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, resolve(await deriveOutput(path)));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_chd_extract", extractArgs(rep.input, rep.output)) : "";
    await batch.start(queue, result);
  } else {
    const args = extractArgs(input.value, output.value);
    commandLine.value = buildCliCommand("cmd_chd_extract", args);
    await run("cmd_chd_extract", args);
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Extract CHD"
      description="Extract CHD files back to their disc images: BIN/CUE for CD-mode, ISO for DVD-mode (PS2/PSP). Drop multiple .chd files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="!!result"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <template v-if="isBatch">
          <BatchFileList
            :items="queue"
            :current-index="batch.currentIndex.value"
            :running="batch.running.value"
            :progress="batch.progress"
            @remove="store.removeFromQueue"
            @clear="store.clearQueue"
          />

          <FileDropZone
            label="Add more CHD files"
            model-value=""
            :multiple="true"
            :filters="[{ name: 'CHD', extensions: ['chd'] }]"
            @update:model-value="async (p: string) => { if (p) store.addToQueue(p, resolve(await deriveOutput(p))) }"
            @update:files="handleFiles"
          />
        </template>

        <template v-else>
          <div class="grid gap-5 lg:grid-cols-2">
            <FileDropZone
              :model-value="input"
              label="Input CHD file"
              :multiple="true"
              :filters="[{ name: 'CHD', extensions: ['chd'] }]"
              :primary="true"
              @update:model-value="handleSingleFile"
              @update:files="handleFiles"
            />

            <FileDropZone
              v-model="output"
              label="Output file (auto-filled)"
              :save-dialog="true"
              :filters="[{ name: 'Disc image', extensions: ['cue', 'iso'] }]"
            />
          </div>
        </template>

        <FileDropZone
          v-model="parent"
          label="Parent CHD (optional)"
          :filters="[{ name: 'CHD', extensions: ['chd'] }]"
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
          :disabled="isBatch ? queue.every(i => i.status !== 'pending') : !input || !output"
          @click="execute"
          @cancel="isBatch ? batch.abort() : abort()"
        >
          {{ isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Extract All (${queue.filter(i => i.status === 'pending').length})` : 'Extract' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :cancelled="cancelled ? 'Operation cancelled.' : undefined" :error="error" />
    </div>
  </div>
</template>

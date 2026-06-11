<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { storeToRefs } from "pinia";
import { useChdExtractStore } from "~/stores/chd-extract";

const store = useChdExtractStore();
const { input, output, parent, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("chd-extract");

const isBatch = computed(() => queue.value.length > 0);

const batch = useBatchOperation("chd-extract", "cmd_chd_extract", (item) => ({
  input: item.input,
  output: item.output,
  parent: parent.value || null,
}));

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

watch(input, async (val) => {
  if (val) output.value = await deriveOutput(val);
});

async function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, await deriveOutput(p));
  }
}

async function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, await deriveOutput(path));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    await batch.start(queue, result);
  } else {
    await run("cmd_chd_extract", {
      input: input.value,
      output: output.value,
      parent: parent.value || null,
    });
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
        <!-- Batch mode -->
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
            @update:model-value="async (p: string) => { if (p) store.addToQueue(p, await deriveOutput(p)) }"
            @update:files="handleFiles"
          />
        </template>

        <!-- Single mode: 2-col on large screens -->
        <template v-else>
          <div class="grid gap-5 lg:grid-cols-2">
            <FileDropZone
              :model-value="input"
              label="Input CHD File"
              :multiple="true"
              :filters="[{ name: 'CHD', extensions: ['chd'] }]"
              :primary="true"
              @update:model-value="handleSingleFile"
              @update:files="handleFiles"
            />

            <FileDropZone
              v-model="output"
              label="Output (auto-derived)"
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

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton
          :loading="loading || batch.running.value"
          :disabled="isBatch ? queue.every(i => i.status !== 'pending') : !input || !output"
          @click="execute"
        >
          {{ isBatch ? `Extract ${queue.filter(i => i.status === 'pending').length} Files` : 'Extract' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="result" :error="error" />
    </div>
  </div>
</template>

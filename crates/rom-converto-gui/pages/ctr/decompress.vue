<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCtrDecompressStore } from "~/stores/ctr-decompress";

const store = useCtrDecompressStore();
const { input, output, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("decompress");

const isBatch = computed(() => queue.value.length > 0);

const batch = useBatchOperation("decompress", "cmd_decompress_rom", (item) => ({
  input: item.input,
  output: item.output || null,
}));

watch(input, (val) => {
  if (val) output.value = deriveDecompressedPath(val);
});

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, deriveDecompressedPath(p));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, deriveDecompressedPath(path));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    await batch.start(queue, result);
  } else {
    await run("cmd_decompress_rom", {
      input: input.value,
      output: output.value || null,
    });
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Decompress ROM"
      description="Decompress Z3DS files back to their original ROM format. Drop multiple files for batch processing."
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
            label="Add more files"
            model-value=""
            :multiple="true"
            :filters="[{ name: 'Z3DS', extensions: ['zcia', 'zcci', 'zcxi', 'z3dsx'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, deriveDecompressedPath(p)) }"
            @update:files="handleFiles"
          />
        </template>

        <!-- Single mode: 2-col on large screens -->
        <div v-else class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            :model-value="input"
            label="Input Z3DS File"
            :multiple="true"
            :filters="[{ name: 'Z3DS', extensions: ['zcia', 'zcci', 'zcxi', 'z3dsx'] }]"
            :primary="true"
            @update:model-value="handleSingleFile"
            @update:files="handleFiles"
          />

          <FileDropZone
            v-model="output"
            label="Output (auto-derived)"
            :save-dialog="true"
            :filters="[{ name: '3DS ROM', extensions: ['cia', 'cci', 'cxi', '3dsx'] }]"
          />
        </div>

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <RunButton
          :loading="loading || batch.running.value"
          :disabled="isBatch ? queue.every(i => i.status !== 'pending') : !input"
          @click="execute"
        >
          {{ isBatch ? `Decompress ${queue.filter(i => i.status === 'pending').length} Files` : 'Decompress' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="result" :error="error" />
    </div>
  </div>
</template>

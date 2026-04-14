<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useDolDecompressStore } from "~/stores/dol-decompress";

const store = useDolDecompressStore();
const { input, output, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("decompress-disc");

const isBatch = computed(() => queue.value.length > 0);

const batch = useBatchOperation("decompress-disc", "cmd_decompress_disc", (item) => ({
  input: item.input,
  output: item.output || null,
}));

watch(input, (val) => {
  if (val) output.value = deriveDiscIsoPath(val);
});

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, deriveDiscIsoPath(p));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, deriveDiscIsoPath(path));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    await batch.start(queue, result);
  } else {
    await run("cmd_decompress_disc", {
      input: input.value,
      output: output.value || null,
    });
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Decompress GameCube RVZ"
      description="Decompress an RVZ file back to a raw GameCube ISO."
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
            label="Add more files"
            model-value=""
            :multiple="true"
            :filters="[{ name: 'RVZ', extensions: ['rvz'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, deriveDiscIsoPath(p)) }"
            @update:files="handleFiles"
          />
        </template>

        <div v-else class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            :model-value="input"
            label="Input RVZ"
            :multiple="true"
            :filters="[{ name: 'RVZ', extensions: ['rvz'] }]"
            :primary="true"
            @update:model-value="handleSingleFile"
            @update:files="handleFiles"
          />

          <FileDropZone
            v-model="output"
            label="Output (auto-derived)"
            :save-dialog="true"
            :filters="[{ name: 'GameCube disc', extensions: ['iso', 'gcm'] }]"
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

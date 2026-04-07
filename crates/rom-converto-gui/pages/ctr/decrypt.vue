<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCtrDecryptStore } from "~/stores/ctr-decrypt";

const store = useCtrDecryptStore();
const { input, output, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("decrypt");

const isBatch = computed(() => queue.value.length > 0);

const batch = useBatchOperation("decrypt", "cmd_decrypt_rom", (item) => ({
  input: item.input,
  output: item.output,
}));

watch(input, (val) => {
  if (val) output.value = deriveDecryptedPath(val);
});

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, deriveDecryptedPath(p));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, deriveDecryptedPath(path));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    await batch.start(queue, result);
  } else {
    await run("cmd_decrypt_rom", {
      input: input.value,
      output: output.value,
    });
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Decrypt ROM"
      description="Decrypt encrypted 3DS ROM files (.cia, .3ds, .cci, .cxi). Drop multiple files for batch processing."
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
            :filters="[{ name: '3DS ROM', extensions: ['cia', '3ds', 'cci', 'cxi'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, deriveDecryptedPath(p)) }"
            @update:files="handleFiles"
          />
        </template>

        <!-- Single mode: 2-col on large screens -->
        <div v-else class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            :model-value="input"
            label="Input ROM"
            :multiple="true"
            :filters="[{ name: '3DS ROM', extensions: ['cia', '3ds', 'cci', 'cxi'] }]"
            :primary="true"
            @update:model-value="handleSingleFile"
            @update:files="handleFiles"
          />

          <FileDropZone
            v-model="output"
            label="Output Path"
            :save-dialog="true"
            :filters="[{ name: '3DS ROM', extensions: ['cia', '3ds', 'cci', 'cxi'] }]"
          />
        </div>

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
          {{ isBatch ? `Decrypt ${queue.filter(i => i.status === 'pending').length} Files` : 'Decrypt' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="result" :error="error" />
    </div>
  </div>
</template>

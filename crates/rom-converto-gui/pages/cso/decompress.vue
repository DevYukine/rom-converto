<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCsoDecompressStore } from "~/stores/cso-decompress";

const store = useCsoDecompressStore();
const { input, output, force, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("cso-decompress");

const isBatch = computed(() => queue.value.length > 0);

const batch = useBatchOperation("cso-decompress", "cmd_cso_decompress", (item) => ({
  inputPath: item.input,
  output: item.output,
  force: force.value,
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
    await run("cmd_cso_decompress", {
      inputPath: input.value,
      output: output.value,
      force: force.value,
    });
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Decompress CSO/ZSO"
      description="Restore the original ISO from a CSO or ZSO container. Drop multiple files for batch processing."
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
            label="Add more CSO/ZSO files"
            model-value=""
            :multiple="true"
            :filters="[{ name: 'Compressed ISO', extensions: ['cso', 'zso'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, deriveDiscIsoPath(p)) }"
            @update:files="handleFiles"
          />
        </template>

        <!-- Single mode: 2-col on large screens -->
        <template v-else>
          <div class="grid gap-5 lg:grid-cols-2">
            <FileDropZone
              :model-value="input"
              label="Input CSO/ZSO file"
              :multiple="true"
              :filters="[{ name: 'Compressed ISO', extensions: ['cso', 'zso'] }]"
              :primary="true"
              @update:model-value="handleSingleFile"
              @update:files="handleFiles"
            />

            <FileDropZone
              v-model="output"
              label="Output (auto-derived)"
              :save-dialog="true"
              :filters="[{ name: 'ISO image', extensions: ['iso'] }]"
            />
          </div>
        </template>

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <FlagToggle
            v-model="force"
            label="Force Overwrite"
            description="Overwrite output file if it already exists"
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

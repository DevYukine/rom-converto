<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useRvlCompressStore } from "~/stores/rvl-compress";

const store = useRvlCompressStore();
const { input, output, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("compress-disc");

const isBatch = computed(() => queue.value.length > 0);

const batch = useBatchOperation("compress-disc", "cmd_compress_disc", (item) => ({
  input: item.input,
  output: item.output || null,
}));

watch(input, (val) => {
  if (val) output.value = deriveRvzPath(val);
});

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, deriveRvzPath(p));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, deriveRvzPath(path));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    await batch.start(queue, result);
  } else {
    await run("cmd_compress_disc", {
      input: input.value,
      output: output.value || null,
    });
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Compress Wii Disc"
      description="Compress a Wii disc image (.iso / .wbfs) to Dolphin's RVZ format. Drop multiple files for batch processing."
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
            :filters="[{ name: 'Wii disc', extensions: ['iso', 'wbfs'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, deriveRvzPath(p)) }"
            @update:files="handleFiles"
          />
        </template>

        <div v-else class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            :model-value="input"
            label="Input disc"
            :multiple="true"
            :filters="[{ name: 'Wii disc', extensions: ['iso', 'wbfs'] }]"
            :primary="true"
            @update:model-value="handleSingleFile"
            @update:files="handleFiles"
          />

          <FileDropZone
            v-model="output"
            label="Output (auto-derived)"
            :save-dialog="true"
            :filters="[{ name: 'RVZ', extensions: ['rvz'] }]"
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
          {{ isBatch ? `Compress ${queue.filter(i => i.status === 'pending').length} Files` : 'Compress' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="result" :error="error" />
    </div>
  </div>
</template>

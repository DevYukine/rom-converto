<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCtrDecryptStore } from "~/stores/ctr-decrypt";

const store = useCtrDecryptStore();
const { input, output, result, error, loading, queue } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run } = useOperation({ result, error, loading });
const progress = useProgress("decrypt");

const isBatch = computed(() => queue.value.length > 0);

const commandLine = ref("");

function decryptArgs(inputPath: string, outputPath: string) {
  return { input: inputPath, output: outputPath };
}

const batch = useBatchOperation("decrypt", "cmd_decrypt_rom", (item) =>
  decryptArgs(item.input, item.output),
);

watch([input, outputDir], () => {
  if (input.value) output.value = resolve(deriveDecryptedPath(input.value));
});

watch(outputDir, () => {
  for (const it of queue.value) {
    if (it.status === "pending") it.output = resolve(deriveDecryptedPath(it.input));
  }
});

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, resolve(deriveDecryptedPath(p)));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, resolve(deriveDecryptedPath(path)));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_decrypt_rom", decryptArgs(rep.input, rep.output)) : "";
    await batch.start(queue, result);
  } else {
    const args = decryptArgs(input.value, output.value);
    commandLine.value = buildCliCommand("cmd_decrypt_rom", args);
    await run("cmd_decrypt_rom", args);
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
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, resolve(deriveDecryptedPath(p))) }"
            @update:files="handleFiles"
          />
        </template>

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
            label="Output file (auto-filled)"
            :save-dialog="true"
            :filters="[{ name: '3DS ROM', extensions: ['cia', '3ds', 'cci', 'cxi'] }]"
          />
        </div>

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
          :disabled="isBatch ? queue.every(i => i.status !== 'pending') : !input"
          @click="execute"
        >
          {{ isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Decrypt All (${queue.filter(i => i.status === 'pending').length})` : 'Decrypt' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :error="error" />
    </div>
  </div>
</template>

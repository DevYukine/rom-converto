<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCtrConvertStore } from "~/stores/ctr-convert";

const store = useCtrConvertStore();
const { input, output, onConflict, result, error, loading, queue } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("ctr-convert");

const isBatch = computed(() => queue.value.length > 0);

const commandLine = ref("");

function convertArgs(inputPath: string, outputPath: string) {
  return { input: inputPath, output: outputPath || null, onConflict: onConflict.value };
}

const batch = useBatchOperation("ctr-convert", "cmd_convert_ctr", (item) =>
  convertArgs(item.input, item.output),
);

function getExt(path: string): string {
  const dot = path.lastIndexOf(".");
  if (dot === -1) return "";
  return path.slice(dot + 1).toLowerCase();
}

const direction = computed(() => {
  const ext = getExt(input.value);
  if (ext === "cia") return "CIA -> 3DS";
  if (ext === "3ds" || ext === "cci") return "3DS -> CIA";
  return "";
});

watch([input, outputDir], () => {
  if (input.value) output.value = resolve(deriveConvertedPath(input.value));
});

watch(outputDir, () => {
  for (const it of queue.value) {
    if (it.status === "pending") it.output = resolve(deriveConvertedPath(it.input));
  }
});

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, resolve(deriveConvertedPath(p)));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, resolve(deriveConvertedPath(path)));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_convert_ctr", convertArgs(rep.input, rep.output)) : "";
    await batch.start(queue, result);
  } else {
    const args = convertArgs(input.value, output.value);
    commandLine.value = buildCliCommand("cmd_convert_ctr", args);
    await run("cmd_convert_ctr", args);
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Convert ROM"
      description="Convert between CIA and CCI/3DS formats. Direction is auto-detected from the input extension."
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
            :filters="[{ name: '3DS ROM', extensions: ['cia', '3ds', 'cci'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, resolve(deriveConvertedPath(p))) }"
            @update:files="handleFiles"
          />
        </template>

        <div v-else class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            :model-value="input"
            label="Input ROM"
            :multiple="true"
            :filters="[{ name: '3DS ROM', extensions: ['cia', '3ds', 'cci'] }]"
            :primary="true"
            @update:model-value="handleSingleFile"
            @update:files="handleFiles"
          />

          <FileDropZone
            v-model="output"
            label="Output file (auto-filled)"
            :save-dialog="true"
            :filters="[{ name: '3DS ROM', extensions: ['cia', '3ds', 'cci'] }]"
          />
        </div>

        <div v-if="direction && !isBatch" class="text-xs text-zinc-400">
          Direction: <span class="font-medium text-sky-300">{{ direction }}</span>
        </div>

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <ConflictPolicyControl v-model="onConflict" />
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
          @cancel="isBatch ? batch.abort() : abort()"
        >
          {{ isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Convert All (${queue.filter(i => i.status === 'pending').length})` : 'Convert' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :cancelled="cancelled ? 'Operation cancelled.' : undefined" :error="error" />
    </div>
  </div>
</template>

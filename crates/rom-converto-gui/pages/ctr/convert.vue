<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCtrConvertStore } from "~/stores/ctr-convert";

const store = useCtrConvertStore();
const { input, output, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("ctr-convert");

const isBatch = computed(() => queue.value.length > 0);

const batch = useBatchOperation("ctr-convert", "cmd_convert_ctr", (item) => ({
  input: item.input,
  output: item.output || null,
}));

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

watch(input, (val) => {
  if (val) output.value = deriveConvertedPath(val);
});

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, deriveConvertedPath(p));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, deriveConvertedPath(path));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    await batch.start(queue, result);
  } else {
    await run("cmd_convert_ctr", {
      input: input.value,
      output: output.value || null,
    });
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
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, deriveConvertedPath(p)) }"
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
            label="Output Path"
            :save-dialog="true"
            :filters="[{ name: '3DS ROM', extensions: ['cia', '3ds', 'cci'] }]"
          />
        </div>

        <div v-if="direction && !isBatch" class="text-xs text-zinc-400">
          Direction: <span class="font-medium text-sky-300">{{ direction }}</span>
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
          {{ isBatch ? `Convert ${queue.filter(i => i.status === 'pending').length} Files` : 'Convert' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :result="result" :error="error" />
    </div>
  </div>
</template>

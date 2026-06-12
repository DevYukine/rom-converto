<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useRvlCompressStore } from "~/stores/rvl-compress";

const store = useRvlCompressStore();
const { input, output, level, chunkSize, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("rvl-compress");

const CHUNK_SIZES = [32768, 65536, 131072, 262144, 524288, 1048576, 2097152];

const isBatch = computed(() => queue.value.length > 0);

const batch = useBatchOperation("rvl-compress", "cmd_compress_disc", (item) => ({
  input: item.input,
  output: item.output || null,
  level: level.value,
  chunkSize: chunkSize.value,
  taskId: "rvl-compress",
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
      level: level.value,
      chunkSize: chunkSize.value,
      taskId: "rvl-compress",
    });
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Compress Wii Disc"
      description="Compress a Wii disc image (.iso / .wbfs) to Dolphin's RVZ format. Legacy WIA, GCZ, and NKit images are detected automatically, verified, and migrated. Drop multiple files for batch processing."
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
            :filters="[{ name: 'Wii disc', extensions: ['iso', 'wbfs', 'wia', 'gcz'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, deriveRvzPath(p)) }"
            @update:files="handleFiles"
          />
        </template>

        <div v-else class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            :model-value="input"
            label="Input disc"
            :multiple="true"
            :filters="[{ name: 'Wii disc', extensions: ['iso', 'wbfs', 'wia', 'gcz'] }]"
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

        <div class="space-y-3 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <label class="flex flex-col gap-1.5">
            <span class="text-sm font-medium text-zinc-200">Zstd level</span>
            <span class="text-xs text-zinc-400">
              1 is fastest, 22 is max ratio. Dolphin's documented suggestion is 5.
            </span>
            <div class="flex items-center gap-3 pt-1">
              <input
                v-model.number="level"
                type="range"
                min="1"
                max="22"
                step="1"
                class="flex-1 accent-sky-500"
              />
              <span class="w-16 shrink-0 text-right font-mono text-sm text-zinc-200">{{ level }}</span>
            </div>
          </label>
          <label class="flex flex-col gap-1.5">
            <span class="text-sm font-medium text-zinc-200">Chunk size (bytes)</span>
            <span class="text-xs text-zinc-400">
              Must be a power of two between 32768 (32 KiB) and 2097152 (2 MiB). Defaults to 131072 (128 KiB), which matches Dolphin's default.
            </span>
            <select v-model.number="chunkSize" class="mt-1 rounded-md border border-zinc-700 bg-zinc-800/50 px-3 py-1.5 text-sm text-zinc-200">
              <option v-for="size in CHUNK_SIZES" :key="size" :value="size">{{ size }}</option>
            </select>
          </label>
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

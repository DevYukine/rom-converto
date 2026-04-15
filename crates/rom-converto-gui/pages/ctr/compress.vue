<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCtrCompressStore } from "~/stores/ctr-compress";

const store = useCtrCompressStore();
const { input, output, level, result, error, loading, queue } = storeToRefs(store);
const { run } = useOperation({ result, error, loading });
const progress = useProgress("compress");

const isBatch = computed(() => queue.value.length > 0);

const batch = useBatchOperation("compress", "cmd_compress_rom", (item) => ({
  input: item.input,
  output: item.output || null,
  level: level.value,
}));

watch(input, (val) => {
  if (val) output.value = deriveCompressedPath(val);
});

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, deriveCompressedPath(p));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, deriveCompressedPath(path));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    await batch.start(queue, result);
  } else {
    await run("cmd_compress_rom", {
      input: input.value,
      output: output.value || null,
      level: level.value,
    });
  }
}
</script>

<template>
  <div>
    <PageHeader
      title="Compress ROM"
      description="Compress decrypted 3DS ROMs to Z3DS format (.zcia, .zcci, .zcxi, .z3dsx). Drop multiple files for batch processing."
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
            :filters="[{ name: '3DS ROM', extensions: ['cia', 'cci', '3ds', 'cxi', '3dsx'] }]"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, deriveCompressedPath(p)) }"
            @update:files="handleFiles"
          />
        </template>

        <!-- Single mode: 2-col on large screens -->
        <div v-else class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            :model-value="input"
            label="Input ROM"
            :multiple="true"
            :filters="[{ name: '3DS ROM', extensions: ['cia', 'cci', '3ds', 'cxi', '3dsx'] }]"
            :primary="true"
            @update:model-value="handleSingleFile"
            @update:files="handleFiles"
          />

          <FileDropZone
            v-model="output"
            label="Output (auto-derived)"
            :save-dialog="true"
            :filters="[{ name: 'Z3DS', extensions: ['zcia', 'zcci', 'zcxi', 'z3dsx'] }]"
          />
        </div>

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <label class="flex flex-col gap-1.5">
            <span class="text-sm font-medium text-zinc-200">Zstd level</span>
            <span class="text-xs text-zinc-400">
              0 uses the library default. 1 is fastest, 22 is max ratio.
              Higher levels produce smaller files at the cost of compression time.
            </span>
            <div class="flex items-center gap-3 pt-1">
              <input
                v-model.number="level"
                type="range"
                min="0"
                max="22"
                step="1"
                class="flex-1 accent-sky-500"
              />
              <span class="w-16 shrink-0 text-right font-mono text-sm text-zinc-200">
                {{ level === 0 ? "default" : level }}
              </span>
            </div>
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

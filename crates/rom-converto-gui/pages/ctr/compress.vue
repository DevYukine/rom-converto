<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCtrCompressStore } from "~/stores/ctr-compress";

const store = useCtrCompressStore();
const { input, output, level, allowEncrypted, result, error, loading, queue } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("compress");

const isBatch = computed(() => queue.value.length > 0);

const commandLine = ref("");

function compressArgs(inputPath: string, outputPath: string) {
  return { input: inputPath, output: outputPath || null, level: level.value, allowEncrypted: allowEncrypted.value };
}

const batch = useBatchOperation("compress", "cmd_compress_rom", (item) =>
  compressArgs(item.input, item.output),
);

watch([input, outputDir], () => {
  if (input.value) output.value = resolve(deriveCompressedPath(input.value));
});

watch(outputDir, () => {
  for (const it of queue.value) {
    if (it.status === "pending") it.output = resolve(deriveCompressedPath(it.input));
  }
});

function handleFiles(paths: string[]) {
  for (const p of paths) {
    store.addToQueue(p, resolve(deriveCompressedPath(p)));
  }
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path, resolve(deriveCompressedPath(path)));
  } else {
    input.value = path;
  }
}

async function execute() {
  progress.reset();
  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_compress_rom", compressArgs(rep.input, rep.output)) : "";
    await batch.start(queue, result);
  } else {
    const args = compressArgs(input.value, output.value);
    commandLine.value = buildCliCommand("cmd_compress_rom", args);
    await run("cmd_compress_rom", args);
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
            @update:model-value="(p: string) => { if (p) store.addToQueue(p, resolve(deriveCompressedPath(p))) }"
            @update:files="handleFiles"
          />
        </template>

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
            label="Output file (auto-filled)"
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

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <FlagToggle
            v-model="allowEncrypted"
            label="Allow encrypted input"
            description="Compress even if the ROM appears encrypted. Encrypted 3DS ROMs barely compress; decrypt first for real savings."
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
          @cancel="isBatch ? batch.abort() : abort()"
        >
          {{ isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Compress All (${queue.filter(i => i.status === 'pending').length})` : 'Compress' }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :cancelled="cancelled ? 'Operation cancelled.' : undefined" :error="error" />
    </div>
  </div>
</template>

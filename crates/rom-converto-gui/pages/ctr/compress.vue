<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCtrCompressStore } from "~/stores/ctr-compress";

const store = useCtrCompressStore();
const { input, output, level, allowEncrypted, onConflict, skipSpaceCheck, outputTemplate, result, error, loading, queue, recursive, maxDepth } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { expand } = useFolderScan(["cia", "cci", "3ds", "cxi", "3dsx"]);
const scanDepth = () => (recursive.value ? maxDepth.value : 1);
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("compress");

const previewMode = ref(false);
const { preview, single: previewSingle, batch: previewBatch, error: previewError } = usePreview("cmd_compress_rom");

const isBatch = computed(() => queue.value.length > 0);
const { canRun, runBlockReason, templateActive } = usePageGating({ input, queue, outputTemplate, templateSingleOnly: true });

const commandLine = ref("");

function compressArgs(inputPath: string, outputPath: string) {
  // Output template is single-file only for CTR, mirroring the CLI.
  const tmpl = !isBatch.value && outputTemplate.value ? outputTemplate.value : null;
  return {
    input: inputPath,
    output: tmpl ? null : outputPath || null,
    level: level.value,
    allowEncrypted: allowEncrypted.value,
    onConflict: onConflict.value,
    skipSpaceCheck: skipSpaceCheck.value,
    outputTemplate: tmpl,
    dryRun: previewMode.value,
  };
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

async function handleFiles(paths: string[]) {
  for (const p of paths) {
    for (const f of await expand(p, scanDepth())) {
      store.addToQueue(f, resolve(deriveCompressedPath(f)));
    }
  }
}

async function handleSingleFile(path: string) {
  const found = await expand(path, scanDepth());
  if (found.length === 1 && found[0] === path && queue.value.length === 0) {
    input.value = path;
  } else {
    for (const f of found) {
      store.addToQueue(f, resolve(deriveCompressedPath(f)));
    }
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

async function runPreview() {
  const rep = isBatch.value ? (queue.value.find((i) => i.status === "pending") ?? queue.value[0]) : null;
  commandLine.value = isBatch.value
    ? rep ? buildCliCommand("cmd_compress_rom", compressArgs(rep.input, rep.output)) : ""
    : buildCliCommand("cmd_compress_rom", compressArgs(input.value, output.value));
  if (isBatch.value) {
    await previewBatch(queue, (item) => compressArgs(item.input, item.output));
  } else {
    await previewSingle(compressArgs(input.value, output.value));
  }
  if (previewError.value) error.value = previewError.value;
}

function onRun() {
  if (previewMode.value) runPreview();
  else execute();
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
            @update:model-value="(p: string) => { if (p) handleSingleFile(p) }"
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

          <InfoTooltip v-if="templateActive" :message="OUTPUT_TEMPLATE_TOOLTIP" block>
            <FileDropZone
              v-model="output"
              class="w-full"
              label="Output file (auto-filled)"
              :save-dialog="true"
              :disabled="true"
              :filters="[{ name: 'Z3DS', extensions: ['zcia', 'zcci', 'zcxi', 'z3dsx'] }]"
            />
          </InfoTooltip>
          <FileDropZone
            v-else
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

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <ConflictPolicyControl v-model="onConflict" />
          <RecursiveOptions
            :recursive="recursive"
            :max-depth="maxDepth"
            @update:recursive="recursive = $event"
            @update:max-depth="maxDepth = $event"
          />
          <FlagToggle
            v-model="skipSpaceCheck"
            label="Skip free space check"
            description="Proceed even if the output filesystem looks too full to hold the result."
          />
        </div>

        <label v-if="!isBatch" class="flex flex-col gap-1.5">
          <span class="text-sm font-medium text-zinc-200">Output template (optional)</span>
          <span class="text-xs text-zinc-400">
            Build the output path from metadata tokens, for example {console}/{title}.{ext}. Single file only. Replaces the explicit output path.
          </span>
          <input
            v-model="outputTemplate"
            type="text"
            placeholder="e.g. {console}/{title}.{ext}"
            class="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-800/50 px-3 py-1.5 text-sm text-zinc-200"
          />
        </label>

        <OutputDirField v-model="outputDir" />

        <ProgressBar
          :percent="progress.percent.value"
          :message="progress.message.value"
          :running="progress.running.value"
        />

        <FlagToggle
          v-model="previewMode"
          label="Preview (dry run)"
          description="Show what each file would do without writing anything."
        />

        <RunButton
          :loading="loading || batch.running.value"
          :batch-current="batch.currentIndex.value"
          :batch-total="queue.length"
          :disabled="!canRun"
          :disabled-reason="runBlockReason"
          @click="onRun"
          @cancel="isBatch ? batch.abort() : abort()"
        >
          {{ previewMode ? 'Preview' : (isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Compress All (${queue.filter(i => i.status === 'pending').length})` : 'Compress') }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :preview="preview" :cancelled="cancelled ? 'Operation cancelled.' : undefined" :error="error" />
    </div>
  </div>
</template>

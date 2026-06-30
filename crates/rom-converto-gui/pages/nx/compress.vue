<script setup lang="ts">
import { open } from "@tauri-apps/plugin-dialog";
import { storeToRefs } from "pinia";
import { isXciInput, useNxCompressStore, type NxMode } from "~/stores/nx-compress";
import type { ReportRecord, RunOutcome } from "~/types/report";

const store = useNxCompressStore();
const { queue, output, keys, level, mode, blockSizeExp, onConflict, skipSpaceCheck, outputTemplate, reportFile, result, error, loading, recursive, maxDepth } =
  storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { expand } = useFolderScan(["nsp", "xci"]);
const scanDepth = () => (recursive.value ? maxDepth.value : 1);
const progress = useProgress("nx-compress");

const previewMode = ref(false);
const { preview, batch: previewBatch, error: previewError } = usePreview("cmd_nx_compress");

const MODE_OPTIONS = [
  { label: "Solid", value: "solid" },
  { label: "Block", value: "block" },
];

const commandLine = ref("");

function compressArgs(item: { input: string; output: string }) {
  const tmpl = outputTemplate.value || null;
  return {
    input: item.input,
    output: tmpl ? null : item.output || null,
    keys: keys.value || null,
    level: level.value,
    mode: mode.value,
    blockSizeExp: blockSizeExp.value,
    onConflict: onConflict.value,
    skipSpaceCheck: skipSpaceCheck.value,
    outputTemplate: tmpl,
    report: !!reportFile.value,
    reportFile: reportFile.value || null,
    dryRun: previewMode.value,
  };
}

const batch = useBatchOperation("nx-compress", "cmd_nx_compress", compressArgs);

const dropZoneRef = ref<HTMLElement | null>(null);
let zoneId: string | null = null;

async function addPaths(paths: string[]) {
  for (const p of paths) {
    if (!p) continue;
    for (const f of await expand(p, scanDepth())) {
      store.addToQueue(f);
    }
  }
  if (!output.value && queue.value.length > 0) {
    const first = queue.value[0];
    if (first) output.value = resolve(deriveNszPath(first.input));
  }
}

watch(outputDir, () => {
  if (queue.value.length === 1) {
    const first = queue.value[0];
    if (first) output.value = resolve(deriveNszPath(first.input));
  }
  for (const it of queue.value) {
    if (it.status === "pending") it.output = resolve(deriveNszPath(it.input));
  }
});

onMounted(() => {
  if (dropZoneRef.value) {
    zoneId = registerDropZone(dropZoneRef.value, addPaths, 0);
  }
});

onUnmounted(() => {
  if (zoneId) unregisterDropZone(zoneId);
});

async function browseInputs() {
  const result = await open({
    directory: false,
    multiple: true,
    filters: [{ name: "Switch container", extensions: ["nsp", "xci"] }],
  });
  if (!result) return;
  addPaths(Array.isArray(result) ? result : [result]);
}

const hasXci = computed(() => queue.value.some((i) => isXciInput(i.input)));
const { canRun, runBlockReason, templateActive } = usePageGating({
  queue,
  outputTemplate,
  emptyInputReason: "Add at least one file to the queue to continue.",
});

async function execute() {
  progress.reset();
  if (!outputTemplate.value) {
    for (const item of queue.value) {
      item.output =
        queue.value.length === 1 ? output.value : resolve(deriveNszPath(item.input));
    }
  }
  const records: ReportRecord[] = [];
  const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
  commandLine.value = rep ? buildCliCommand("cmd_nx_compress", compressArgs(rep)) : "";
  await batch.start(
    queue,
    result,
    { errorRef: error },
    (res) => {
      const record = (res as RunOutcome)?.record;
      if (record) records.push(record);
    },
    async (item, err) => {
      if (reportFile.value) await pushFailedRecord(records, item.input, "compress", err);
    },
  );
  if (reportFile.value && records.length) {
    await writeRunReport(reportFile.value, records);
  }
}

async function runPreview() {
  if (!outputTemplate.value) {
    for (const item of queue.value) {
      item.output = queue.value.length === 1 ? output.value : resolve(deriveNszPath(item.input));
    }
  }
  const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
  commandLine.value = rep ? buildCliCommand("cmd_nx_compress", compressArgs(rep)) : "";
  await previewBatch(queue, compressArgs);
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
      title="Compress to NSZ/XCZ"
      description="Compress NSP into NSZ or XCI into XCZ. Output is nsz-compatible (https://github.com/nicoboss/nsz). Requires prod.keys."
      :loading="loading || batch.running.value"
      :has-result="!!result"
      :has-error="!!error"
    />

    <OperationCard>
      <div class="space-y-5">
        <BatchFileList
          v-if="queue.length > 0"
          :items="queue"
          :current-index="batch.currentIndex.value"
          :running="batch.running.value"
          :progress="batch.progress"
          @remove="store.removeFromQueue"
          @clear="store.clearQueue"
        />

        <div class="space-y-1.5">
          <label class="block text-sm font-medium text-zinc-300">
            {{ queue.length > 0 ? "Add more inputs" : "Inputs" }}
          </label>
          <div
            ref="dropZoneRef"
            class="drop-zone flex cursor-default flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed border-zinc-700 bg-zinc-800/30 px-4 py-6 transition xl:py-8"
          >
            <svg class="h-8 w-8 text-zinc-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
              <path stroke-linecap="round" stroke-linejoin="round" d="M3 16.5v2.25A2.25 2.25 0 005.25 21h13.5A2.25 2.25 0 0021 18.75V16.5m-13.5-9L12 3m0 0l4.5 4.5M12 3v13.5" />
            </svg>
            <span class="text-center text-sm text-zinc-400">
              Drop NSP or XCI files. Multiple inputs queue sequentially.
            </span>
            <button
              type="button"
              class="rounded-md bg-zinc-700/60 px-3 py-1.5 text-xs font-medium text-zinc-200 transition hover:bg-zinc-700"
              @click="browseInputs"
            >
              Browse files
            </button>
          </div>
        </div>

        <InfoTooltip v-if="queue.length <= 1 && templateActive" :message="OUTPUT_TEMPLATE_TOOLTIP" block>
          <FileDropZone
            v-model="output"
            class="w-full"
            label="Output file (auto-filled)"
            :save-dialog="true"
            :disabled="true"
            :filters="[{ name: 'Switch compressed', extensions: ['nsz', 'xcz'] }]"
          />
        </InfoTooltip>
        <FileDropZone
          v-else-if="queue.length <= 1"
          v-model="output"
          label="Output file (auto-filled)"
          :save-dialog="true"
          :filters="[{ name: 'Switch compressed', extensions: ['nsz', 'xcz'] }]"
        />
        <div
          v-else
          class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3 text-xs text-zinc-400"
        >
          Multi-file batch: each output is derived from its input (.nsp -> .nsz,
          .xci -> .xcz) next to the source file.
        </div>

        <FileDropZone
          v-model="keys"
          label="prod.keys (optional, falls back to ~/.switch/prod.keys)"
          :filters="[{ name: 'prod.keys', extensions: ['keys', 'txt'] }]"
        />

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3 space-y-3">
          <div>
            <label class="flex flex-col gap-1.5">
              <span class="text-sm font-medium text-zinc-200">Zstd level</span>
              <span class="text-xs text-zinc-400">
                nsz default is 18. 22 is the maximum but needs over 1 GiB of
                RAM during decompression on the Switch and may break installers.
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
                <span class="w-12 shrink-0 text-right font-mono text-sm text-zinc-200">
                  {{ level }}
                </span>
              </div>
            </label>
          </div>

          <div class="space-y-1.5">
            <SegmentedControl
              :model-value="mode"
              label="Mode"
              :options="MODE_OPTIONS"
              @update:model-value="(v: string) => store.setMode(v as NxMode)"
            />
            <p class="text-xs text-zinc-400">
              Solid emits one zstd frame per NCA (smaller, default for NSP).
              Block compresses fixed-size chunks independently (random read
              friendly, default for XCI). XCI input auto-selects block
              unless you change it.
            </p>
          </div>

          <div v-if="mode === 'block'">
            <label class="flex flex-col gap-1.5">
              <span class="text-sm font-medium text-zinc-200">Block size (power of two)</span>
              <span class="text-xs text-zinc-400">
                14 = 16 KiB, 20 = 1 MiB (nsz default), 32 = 4 GiB. Smaller
                blocks parallelize better but inflate the size table.
              </span>
              <div class="flex items-center gap-3 pt-1">
                <input
                  v-model.number="blockSizeExp"
                  type="range"
                  min="14"
                  max="32"
                  step="1"
                  class="flex-1 accent-sky-500"
                />
                <span class="w-24 shrink-0 text-right font-mono text-sm text-zinc-200">
                  2^{{ blockSizeExp }} = {{ (1 << Math.min(blockSizeExp, 30)).toLocaleString() }} B
                </span>
              </div>
            </label>
          </div>
        </div>

        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3 space-y-3">
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

        <label class="flex flex-col gap-1.5">
          <span class="text-sm font-medium text-zinc-200">Output template (optional)</span>
          <span class="text-xs text-zinc-400">
            Build the output path from metadata tokens, for example {console}/{title}.{ext}. Replaces the explicit output path.
          </span>
          <input
            v-model="outputTemplate"
            type="text"
            placeholder="e.g. {console}/{title}.{ext}"
            class="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-800/50 px-3 py-1.5 text-sm text-zinc-200"
          />
        </label>

        <OutputDirField v-model="outputDir" />

        <FileDropZone
          v-model="reportFile"
          label="Run report file (optional)"
          placeholder="No report"
          :save-dialog="true"
          :filters="[{ name: 'Report', extensions: ['csv', 'json', 'html'] }]"
        />

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
          @cancel="batch.abort"
        >
          {{ previewMode ? 'Preview' : (queue.filter(i => i.status === 'pending').length > 1 ? `Compress All (${queue.filter(i => i.status === 'pending').length})` : 'Compress') }}
        </RunButton>

        <div v-if="hasXci && mode === 'solid'" class="text-xs text-amber-300/80">
          Solid mode on XCI is uncommon; XCZ in solid mode forces emulators
          to fully decompress before mounting. Block is recommended for XCI.
        </div>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :preview="preview" :error="error" />
    </div>
  </div>
</template>

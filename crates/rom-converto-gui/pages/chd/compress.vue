<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useChdCompressStore } from "~/stores/chd-compress";
import type { ComparisonSummary, ReportRecord, RunOutcome } from "~/types/report";

const store = useChdCompressStore();
const { input, output, onConflict, skipSpaceCheck, zstd, mode, hunkSize, outputTemplate, reportFile, verifyAfter, result, error, loading, queue, recursive, maxDepth } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { expand } = useFolderScan(["cue", "iso"]);
const scanDepth = () => (recursive.value ? maxDepth.value : 1);
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("chd-compress");

const previewMode = ref(false);
const { preview, single: previewSingle, batch: previewBatch, error: previewError } = usePreview("cmd_chd_compress");

const MODE_OPTIONS = [
  { label: "Auto", value: "auto" },
  { label: "CD", value: "cd" },
  { label: "DVD", value: "dvd" },
];

const isBatch = computed(() => queue.value.length > 0);
const commandLine = ref("");

const { canRun, runBlockReason, templateActive } = usePageGating({ input, queue, outputTemplate });

function chdArgs(inputPath: string, outputPath: string) {
  const tmpl = outputTemplate.value || null;
  return {
    inputPath,
    output: tmpl ? null : outputPath,
    onConflict: onConflict.value,
    skipSpaceCheck: skipSpaceCheck.value,
    zstd: zstd.value,
    mode: mode.value === "auto" ? null : mode.value,
    hunkSize: hunkSize.value || null,
    outputTemplate: tmpl,
    report: !!reportFile.value,
    reportFile: reportFile.value || null,
    verifyAfter: verifyAfter.value,
    dryRun: previewMode.value,
  };
}

const batch = useBatchOperation("chd-compress", "cmd_chd_compress", (item) =>
  chdArgs(item.input, item.output),
);
const comparisons = ref<ComparisonSummary[]>([]);

watch([input, outputDir], () => {
  if (input.value) output.value = resolve(deriveChdPath(input.value));
});

watch(outputDir, () => {
  for (const it of queue.value) {
    if (it.status === "pending") it.output = resolve(deriveChdPath(it.input));
  }
});

async function handleFiles(paths: string[]) {
  for (const p of paths) {
    for (const f of await expand(p, scanDepth())) {
      store.addToQueue(f, resolve(deriveChdPath(f)));
    }
  }
}

async function handleSingleFile(path: string) {
  const found = await expand(path, scanDepth());
  if (found.length === 1 && found[0] === path && queue.value.length === 0) {
    input.value = path;
  } else {
    for (const f of found) {
      store.addToQueue(f, resolve(deriveChdPath(f)));
    }
  }
}

async function execute() {
  progress.reset();
  const records: ReportRecord[] = [];
  comparisons.value = [];
  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_chd_compress", chdArgs(rep.input, rep.output)) : "";
    await batch.start(
      queue,
      result,
      { errorRef: error },
      (res) => {
        const record = (res as RunOutcome)?.record;
        if (record) records.push(record);
        const comparison = (res as RunOutcome)?.comparison;
        if (comparison) comparisons.value.push(comparison);
      },
      async (item, err) => {
        if (reportFile.value) await pushFailedRecord(records, item.input, "compress", err);
      },
    );
  } else {
    const args = chdArgs(input.value, output.value);
    commandLine.value = buildCliCommand("cmd_chd_compress", args);
    await runReportable("cmd_chd_compress", args, { result, error, loading, cancelled }, records, "compress", comparisons.value);
  }
  if (reportFile.value && records.length) {
    await writeRunReport(reportFile.value, records);
  }
}

async function runPreview() {
  const rep = isBatch.value ? (queue.value.find((i) => i.status === "pending") ?? queue.value[0]) : null;
  commandLine.value = isBatch.value
    ? rep ? buildCliCommand("cmd_chd_compress", chdArgs(rep.input, rep.output)) : ""
    : buildCliCommand("cmd_chd_compress", chdArgs(input.value, output.value));
  if (isBatch.value) {
    await previewBatch(queue, (item) => chdArgs(item.input, item.output));
  } else {
    await previewSingle(chdArgs(input.value, output.value));
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
      title="Compress to CHD"
      description="Compress disc images to CHD. CUE/BIN and CD-media ISOs (PS1, PS2-CD) become CD-mode CHDs; PS2-DVD and PSP ISOs become DVD-mode CHDs (media type and hunk size are auto-detected). Drop multiple files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="!!result"
      :has-error="!!error"
    />

    <div class="mb-4">
      <OutputLog :command="commandLine" :result="result" :preview="preview" :cancelled="cancelled ? 'Operation cancelled.' : undefined" :error="error" />
    </div>

    <div class="mb-4">
      <ComparisonList :comparisons="comparisons" />
    </div>

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
            label="Add more disc images"
            model-value=""
            :multiple="true"
            :filters="[{ name: 'Disc image', extensions: ['cue', 'iso'] }]"
            @update:model-value="(p: string) => { if (p) handleSingleFile(p) }"
            @update:files="handleFiles"
          />
        </template>

        <template v-else>
          <div class="grid gap-5 lg:grid-cols-2">
            <FileDropZone
              :model-value="input"
              label="Input disc image (.cue or .iso)"
              :multiple="true"
              :filters="[{ name: 'Disc image', extensions: ['cue', 'iso'] }]"
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
                :filters="[{ name: 'CHD', extensions: ['chd'] }]"
              />
            </InfoTooltip>
            <FileDropZone
              v-else
              v-model="output"
              label="Output file (auto-filled)"
              :save-dialog="true"
              :filters="[{ name: 'CHD', extensions: ['chd'] }]"
            />
          </div>
        </template>

        <div class="space-y-3 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
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
          <FlagToggle
            v-model="zstd"
            label="zstd codec (DVD mode)"
            description="Better ratio, but the CHD is rejected by AetherSX2/NetherSX2. Leave off for maximum compatibility"
          />
          <div class="space-y-1.5">
            <SegmentedControl
              :model-value="mode"
              label="Disc mode"
              :options="MODE_OPTIONS"
              @update:model-value="(v: string) => { mode = v as 'auto' | 'cd' | 'dvd' }"
            />
            <p class="text-xs text-zinc-500">
              Auto picks CD or DVD from the image. Override only when detection is wrong.
            </p>
          </div>
          <label class="flex flex-col gap-1.5">
            <span class="text-sm font-medium text-zinc-200">Hunk size</span>
            <input
              v-model.number="hunkSize"
              type="number"
              placeholder="auto"
              class="mt-1 w-40 rounded-md border border-zinc-700 bg-zinc-800/50 px-3 py-1.5 text-sm text-zinc-200"
            />
          </label>
        </div>

        <label class="flex flex-col gap-1.5">
          <span class="text-sm font-medium text-zinc-200">Output template (optional)</span>
          <span class="text-xs text-zinc-400">
            Build the output path from metadata tokens, for example {console}/{title}.{ext}. Replaces the explicit output path.
          </span>
          <input
            v-model="outputTemplate"
            type="text"
            placeholder="for example, {console}/{title}.{ext}"
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
          v-model="verifyAfter"
          label="Verify after conversion"
          description="Re-check each output after writing it and note whether the format supports a full round-trip decode."
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
          {{ previewMode ? 'Preview' : (isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Compress all (${queue.filter(i => i.status === 'pending').length})` : 'Compress') }}
        </RunButton>
      </div>
    </OperationCard>
  </div>
</template>

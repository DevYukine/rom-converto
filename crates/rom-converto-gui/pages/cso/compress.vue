<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCsoCompressStore } from "~/stores/cso-compress";
import type { ReportRecord, RunOutcome } from "~/types/report";

const store = useCsoCompressStore();
const { input, output, format, onConflict, skipSpaceCheck, blockSize, outputTemplate, reportFile, result, error, loading, queue, recursive, maxDepth } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { expand } = useFolderScan(["iso"]);
const scanDepth = () => (recursive.value ? maxDepth.value : 1);
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("cso-compress");

const previewMode = ref(false);
const { preview, single: previewSingle, batch: previewBatch, error: previewError } = usePreview("cmd_cso_compress");

const isBatch = computed(() => queue.value.length > 0);
const { canRun, runBlockReason, templateActive } = usePageGating({ input, queue, outputTemplate });

const FORMAT_OPTIONS = [
  { label: "CSO (PSP, PPSSPP)", value: "cso" },
  { label: "ZSO (PS2 via OPL)", value: "zso" },
];

const commandLine = ref("");

function csoArgs(inputPath: string, outputPath: string) {
  const tmpl = outputTemplate.value || null;
  return {
    inputPath,
    output: tmpl ? null : outputPath,
    format: format.value,
    onConflict: onConflict.value,
    skipSpaceCheck: skipSpaceCheck.value,
    blockSize: blockSize.value || null,
    outputTemplate: tmpl,
    report: !!reportFile.value,
    reportFile: reportFile.value || null,
    dryRun: previewMode.value,
  };
}

const batch = useBatchOperation("cso-compress", "cmd_cso_compress", (item) =>
  csoArgs(item.input, item.output),
);

watch([input, outputDir], () => {
  if (input.value) output.value = resolve(deriveCsoPath(input.value, format.value));
});

watch(format, (fmt) => {
  if (input.value) output.value = resolve(deriveCsoPath(input.value, fmt));
  for (const item of queue.value) {
    if (item.status === "pending") item.output = resolve(deriveCsoPath(item.input, fmt));
  }
});

watch(outputDir, () => {
  for (const item of queue.value) {
    if (item.status === "pending") item.output = resolve(deriveCsoPath(item.input, format.value));
  }
});

async function handleFiles(paths: string[]) {
  for (const p of paths) {
    for (const f of await expand(p, scanDepth())) {
      store.addToQueue(f, resolve(deriveCsoPath(f, format.value)));
    }
  }
}

async function handleSingleFile(path: string) {
  const found = await expand(path, scanDepth());
  if (found.length === 1 && found[0] === path && queue.value.length === 0) {
    input.value = path;
  } else {
    for (const f of found) {
      store.addToQueue(f, resolve(deriveCsoPath(f, format.value)));
    }
  }
}

async function execute() {
  progress.reset();
  const records: ReportRecord[] = [];
  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_cso_compress", csoArgs(rep.input, rep.output)) : "";
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
  } else {
    const args = csoArgs(input.value, output.value);
    commandLine.value = buildCliCommand("cmd_cso_compress", args);
    await runReportable("cmd_cso_compress", args, { result, error, loading, cancelled }, records, "compress");
  }
  if (reportFile.value && records.length) {
    await writeRunReport(reportFile.value, records);
  }
}

async function runPreview() {
  const rep = isBatch.value ? (queue.value.find((i) => i.status === "pending") ?? queue.value[0]) : null;
  commandLine.value = isBatch.value
    ? rep ? buildCliCommand("cmd_cso_compress", csoArgs(rep.input, rep.output)) : ""
    : buildCliCommand("cmd_cso_compress", csoArgs(input.value, output.value));
  if (isBatch.value) {
    await previewBatch(queue, (item) => csoArgs(item.input, item.output));
  } else {
    await previewSingle(csoArgs(input.value, output.value));
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
      title="Compress to CSO/ZSO"
      description="Compress PSP/PS2 ISOs into block-compressed containers. CSO for PSP hardware and PPSSPP, ZSO for PS2 via Open PS2 Loader. Drop multiple .iso files for batch processing."
      :loading="loading || batch.running.value"
      :has-result="!!result"
      :has-error="!!error"
    />

    <div class="mb-4">
      <OutputLog :command="commandLine" :result="result" :preview="preview" :cancelled="cancelled ? 'Operation cancelled.' : undefined" :error="error" />
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
            label="Add more ISO files"
            model-value=""
            :multiple="true"
            :filters="[{ name: 'ISO image', extensions: ['iso'] }]"
            @update:model-value="(p: string) => { if (p) handleSingleFile(p) }"
            @update:files="handleFiles"
          />
        </template>

        <template v-else>
          <div class="grid gap-5 lg:grid-cols-2">
            <FileDropZone
              :model-value="input"
              label="Input ISO file"
              :multiple="true"
              :filters="[{ name: 'ISO image', extensions: ['iso'] }]"
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
                :filters="[{ name: 'Compressed ISO', extensions: ['cso', 'zso'] }]"
              />
            </InfoTooltip>
            <FileDropZone
              v-else
              v-model="output"
              label="Output file (auto-filled)"
              :save-dialog="true"
              :filters="[{ name: 'Compressed ISO', extensions: ['cso', 'zso'] }]"
            />
          </div>
        </template>

        <div class="space-y-3 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <SegmentedControl
            :model-value="format"
            label="Format"
            :options="FORMAT_OPTIONS"
            @update:model-value="(v: string) => { format = v as 'cso' | 'zso' }"
          />
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
            v-model="previewMode"
            label="Preview (dry run)"
            description="Show what each file would do without writing anything."
          />
          <label class="flex flex-col gap-1.5">
            <span class="text-sm font-medium text-zinc-200">Block size</span>
            <span class="text-xs text-zinc-400">
              Power of two in bytes. Leave blank for the default of 2048, or 16384 for inputs of 2 GiB and beyond (matching maxcso).
            </span>
            <input
              v-model.number="blockSize"
              type="number"
              placeholder="default"
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

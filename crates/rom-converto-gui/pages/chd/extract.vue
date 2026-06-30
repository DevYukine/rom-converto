<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { storeToRefs } from "pinia";
import { useChdExtractStore } from "~/stores/chd-extract";
import type { ReportRecord, RunOutcome } from "~/types/report";

const store = useChdExtractStore();
const { input, output, parent, skipSpaceCheck, outputTemplate, reportFile, result, error, loading, queue, recursive, maxDepth } = storeToRefs(store);
const { outputDir, resolve } = useOutputDir();
const { expand } = useFolderScan(["chd"]);
const scanDepth = () => (recursive.value ? maxDepth.value : 1);
const { run, cancelled, abort } = useOperation({ result, error, loading });
const progress = useProgress("chd-extract");

const previewMode = ref(false);
const { preview, single: previewSingle, batch: previewBatch, error: previewError } = usePreview("cmd_chd_extract");

const isBatch = computed(() => queue.value.length > 0);
const commandLine = ref("");

function extractArgs(inputPath: string, outputPath: string) {
  const tmpl = outputTemplate.value || null;
  return {
    input: inputPath,
    output: tmpl ? null : outputPath,
    parent: parent.value || null,
    skipSpaceCheck: skipSpaceCheck.value,
    outputTemplate: tmpl,
    report: !!reportFile.value,
    reportFile: reportFile.value || null,
    dryRun: previewMode.value,
  };
}

const batch = useBatchOperation("chd-extract", "cmd_chd_extract", (item) =>
  extractArgs(item.input, item.output),
);

// DVD-mode CHDs extract to a single .iso, CD-mode to .cue/.bin;
// the mode is only knowable from the file's metadata.
async function deriveOutput(path: string): Promise<string> {
  try {
    const raw = await invoke<string>("cmd_read_info", { input: path, keys: null });
    const parsed = JSON.parse(raw);
    if (parsed?.dvd) return deriveDiscIsoPath(path);
  } catch {
    // Fall through to the CD default.
  }
  return deriveCuePath(path);
}

watch([input, outputDir], async () => {
  if (input.value) output.value = resolve(await deriveOutput(input.value));
});

watch(outputDir, () => {
  for (const it of queue.value) {
    if (it.status === "pending") it.output = withOutputDir(it.output, outputDir.value);
  }
});

async function handleFiles(paths: string[]) {
  for (const p of paths) {
    for (const f of await expand(p, scanDepth())) {
      store.addToQueue(f, resolve(await deriveOutput(f)));
    }
  }
}

async function handleSingleFile(path: string) {
  const found = await expand(path, scanDepth());
  if (found.length === 1 && found[0] === path && queue.value.length === 0) {
    input.value = path;
  } else {
    for (const f of found) {
      store.addToQueue(f, resolve(await deriveOutput(f)));
    }
  }
}

async function execute() {
  progress.reset();
  const records: ReportRecord[] = [];
  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_chd_extract", extractArgs(rep.input, rep.output)) : "";
    await batch.start(
      queue,
      result,
      undefined,
      (res) => {
        const record = (res as RunOutcome)?.record;
        if (record) records.push(record);
      },
      async (item, err) => {
        if (reportFile.value) await pushFailedRecord(records, item.input, "extract", err);
      },
    );
  } else {
    const args = extractArgs(input.value, output.value);
    commandLine.value = buildCliCommand("cmd_chd_extract", args);
    await runReportable("cmd_chd_extract", args, { result, error, loading, cancelled }, records, "extract");
  }
  if (reportFile.value && records.length) {
    await writeRunReport(reportFile.value, records);
  }
}

async function runPreview() {
  const rep = isBatch.value ? (queue.value.find((i) => i.status === "pending") ?? queue.value[0]) : null;
  commandLine.value = isBatch.value
    ? rep ? buildCliCommand("cmd_chd_extract", extractArgs(rep.input, rep.output)) : ""
    : buildCliCommand("cmd_chd_extract", extractArgs(input.value, output.value));
  if (isBatch.value) {
    await previewBatch(queue, (item) => extractArgs(item.input, item.output));
  } else {
    await previewSingle(extractArgs(input.value, output.value));
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
      title="Extract CHD"
      description="Extract CHD files back to their disc images: BIN/CUE for CD-mode, ISO for DVD-mode (PS2/PSP). Drop multiple .chd files for batch processing."
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
            label="Add more CHD files"
            model-value=""
            :multiple="true"
            :filters="[{ name: 'CHD', extensions: ['chd'] }]"
            @update:model-value="(p: string) => { if (p) handleSingleFile(p) }"
            @update:files="handleFiles"
          />
        </template>

        <template v-else>
          <div class="grid gap-5 lg:grid-cols-2">
            <FileDropZone
              :model-value="input"
              label="Input CHD file"
              :multiple="true"
              :filters="[{ name: 'CHD', extensions: ['chd'] }]"
              :primary="true"
              @update:model-value="handleSingleFile"
              @update:files="handleFiles"
            />

            <FileDropZone
              v-model="output"
              label="Output file (auto-filled)"
              :save-dialog="true"
              :filters="[{ name: 'Disc image', extensions: ['cue', 'iso'] }]"
            />
          </div>
        </template>

        <FileDropZone
          v-model="parent"
          label="Parent CHD (optional)"
          :filters="[{ name: 'CHD', extensions: ['chd'] }]"
        />

        <div class="space-y-3 rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
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
            Build the output path from metadata tokens, for example {console}/{title}.{ext}. The sidecars share the resolved stem. Replaces the explicit output path.
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
          :disabled="isBatch ? queue.every(i => i.status !== 'pending') : !input || (!output && !outputTemplate)"
          @click="onRun"
          @cancel="isBatch ? batch.abort() : abort()"
        >
          {{ previewMode ? 'Preview' : (isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Extract All (${queue.filter(i => i.status === 'pending').length})` : 'Extract') }}
        </RunButton>
      </div>
    </OperationCard>

    <div class="mt-4">
      <OutputLog :command="commandLine" :result="result" :preview="preview" :cancelled="cancelled ? 'Operation cancelled.' : undefined" :error="error" />
    </div>
  </div>
</template>

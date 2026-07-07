<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useDatVerifyStore } from "~/stores/datVerify";
import { invoke } from "@tauri-apps/api/core";
import type { DatResultRow } from "~/components/DatResultList.vue";

const store = useDatVerifyStore();
const { input, quick, result, error, loading, queue } = storeToRefs(store);
const progress = useProgress("dat-verify");

const isBatch = computed(() => queue.value.length > 0);

const ROM_FILTERS = [{ name: "ROM file", extensions: ["*"] }];

interface DatTrackCheck {
  track: number;
  ok: boolean;
  algo: string | null;
  matchedFile: string | null;
}

interface DatVerifyResult {
  kind: "verify";
  path: string;
  verdict: "verified" | "hint" | "unknown" | "unsupported" | "failed";
  matchAlgo: string | null;
  gameName: string | null;
  platform: string | null;
  signatureGroup: string | null;
  datFile: string | null;
  datFileId: string | null;
  datVersion: string | null;
  externalIds: { provider: string; id: string }[];
  tracks: DatTrackCheck[] | null;
  error: string | null;
}

const verifyResult = ref<DatVerifyResult | null>(null);
const batchResults = ref<DatVerifyResult[]>([]);
const commandLine = ref("");

const { canRun, runBlockReason } = usePageGating({
  input,
  queue,
  emptyInputReason: "Select a ROM file to verify against the Playmatch database.",
});

function verifyArgs(inputPath: string) {
  return { input: inputPath, quick: quick.value };
}

const batch = useBatchOperation("dat-verify", "cmd_dat_verify", (item) =>
  verifyArgs(item.input),
);

const allWarnings = computed(() => {
  const seen = new Set(progress.warnings.value);
  for (const slot of batch.progressSlots) for (const w of slot.warnings.value) seen.add(w);
  return [...seen];
});

function handleReorder(ids: string[]) {
  batch.reorder(queue, ids);
}

function handleRemoveSelected(ids: string[]) {
  batch.removeSelected(queue, ids);
}

function handleFiles(paths: string[]) {
  for (const p of paths) store.addToQueue(p);
}

function handleSingleFile(path: string) {
  if (queue.value.length > 0) {
    store.addToQueue(path);
  } else {
    input.value = path;
  }
}

const VERDICT_COLOR: Record<DatVerifyResult["verdict"], "emerald" | "amber" | "zinc" | "red"> = {
  verified: "emerald",
  hint: "amber",
  unknown: "zinc",
  unsupported: "zinc",
  failed: "red",
};

const VERDICT_LABEL: Record<DatVerifyResult["verdict"], string> = {
  verified: "Verified",
  hint: "Name+size hint",
  unknown: "Unknown",
  unsupported: "Unsupported",
  failed: "Failed",
};

function verdictColor(verdict: DatVerifyResult["verdict"]): string {
  return VERDICT_COLOR[verdict];
}

// externalIds arrives pre-filtered from the backend (Automatic/Manual matches only).
function visibleExternalIds(r: DatVerifyResult) {
  return r.externalIds;
}

async function execute() {
  progress.reset();
  verifyResult.value = null;
  batchResults.value = [];
  error.value = "";
  result.value = "";

  if (isBatch.value) {
    const rep = queue.value.find((i) => i.status === "pending") ?? queue.value[0];
    commandLine.value = rep ? buildCliCommand("cmd_dat_verify", verifyArgs(rep.input)) : "";
    await batch.start(queue, result, { errorRef: error });
    for (const item of queue.value) {
      if (item.status === "done" && item.result) {
        try {
          batchResults.value.push(JSON.parse(item.result) as DatVerifyResult);
        } catch {
          // non-JSON result, skip from the structured list
        }
      }
    }
  } else {
    const args = verifyArgs(input.value);
    commandLine.value = buildCliCommand("cmd_dat_verify", args);
    loading.value = true;
    try {
      const json = await invoke<string>("cmd_dat_verify", args);
      const parsed = JSON.parse(json) as DatVerifyResult;
      verifyResult.value = parsed;
      result.value = `${VERDICT_LABEL[parsed.verdict]}${parsed.gameName ? `: ${parsed.gameName}` : ""}`;
    } catch (e: any) {
      const message = typeof e === "string" ? e : e.message || String(e);
      if (message.includes("operation cancelled")) {
        error.value = "";
      } else {
        error.value = message;
      }
    } finally {
      loading.value = false;
    }
  }
}

const batchRows = computed<DatResultRow[]>(() =>
  batchResults.value.map((r) => ({
    key: r.path,
    icon: r.verdict === "verified" ? "ok" : r.verdict === "failed" ? "error" : r.verdict === "hint" ? "warn" : "info",
    primary: basename(r.path),
    secondary: [r.gameName, r.datFile].filter(Boolean).join(" - ") || undefined,
    badge: VERDICT_LABEL[r.verdict],
    badgeColor: VERDICT_COLOR[r.verdict],
  })),
);
</script>

<template>
  <div>
    <PageHeader
      title="Verify"
      description="Verify a ROM's decoded content hashes against the Playmatch database. Container formats are hashed on their decoded inner stream, so results match regardless of compression."
      :loading="loading || batch.running.value"
      :has-result="!!result"
      :has-error="!!error"
    />

    <template v-if="verifyResult">
      <div class="mb-4 space-y-3">
        <div
          class="flex items-center gap-3 rounded-lg border-l-2 px-4 py-3"
          :class="{
            'border-emerald-500 bg-emerald-500/5': verdictColor(verifyResult.verdict) === 'emerald',
            'border-amber-500 bg-amber-500/5': verdictColor(verifyResult.verdict) === 'amber',
            'border-zinc-500 bg-zinc-500/5': verdictColor(verifyResult.verdict) === 'zinc',
            'border-red-500 bg-red-500/5': verdictColor(verifyResult.verdict) === 'red',
          }"
        >
          <span
            class="inline-flex items-center rounded-md px-2.5 py-1 text-sm font-semibold"
            :class="{
              'bg-emerald-500/20 text-emerald-300': verdictColor(verifyResult.verdict) === 'emerald',
              'bg-amber-500/20 text-amber-300': verdictColor(verifyResult.verdict) === 'amber',
              'bg-zinc-700/50 text-zinc-400': verdictColor(verifyResult.verdict) === 'zinc',
              'bg-red-500/20 text-red-300': verdictColor(verifyResult.verdict) === 'red',
            }"
          >
            {{ VERDICT_LABEL[verifyResult.verdict] }}
          </span>
          <span v-if="verifyResult.gameName" class="text-sm text-zinc-400">
            {{ verifyResult.gameName }}
          </span>
        </div>

        <div v-if="verifyResult.tracks" class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <h4 class="mb-2 text-xs font-semibold uppercase tracking-wider text-zinc-500">Track checks</h4>
          <div class="space-y-1.5">
            <CheckRow
              v-for="t in verifyResult.tracks"
              :key="t.track"
              :label="`Track ${t.track}${t.matchedFile ? ` (${t.matchedFile})` : ''}`"
              :valid="t.ok"
            />
          </div>
        </div>

        <div
          v-if="verifyResult.gameName || verifyResult.platform || verifyResult.signatureGroup || verifyResult.datFile || verifyResult.datVersion"
          class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3"
        >
          <dl class="grid grid-cols-2 gap-x-4 gap-y-1.5 text-sm">
            <template v-if="verifyResult.gameName">
              <dt class="text-zinc-500">Game</dt>
              <dd class="text-zinc-300">{{ verifyResult.gameName }}</dd>
            </template>
            <template v-if="verifyResult.platform">
              <dt class="text-zinc-500">Platform</dt>
              <dd class="text-zinc-300">{{ verifyResult.platform }}</dd>
            </template>
            <template v-if="verifyResult.signatureGroup">
              <dt class="text-zinc-500">Signature group</dt>
              <dd class="text-zinc-300">{{ verifyResult.signatureGroup }}</dd>
            </template>
            <template v-if="verifyResult.datFile">
              <dt class="text-zinc-500">DAT file</dt>
              <dd class="text-zinc-300">{{ verifyResult.datFile }}</dd>
            </template>
            <template v-if="verifyResult.datFileId">
              <dt class="text-zinc-500">DAT file id</dt>
              <dd class="font-mono text-xs text-zinc-300">{{ verifyResult.datFileId }}</dd>
            </template>
            <template v-if="verifyResult.datVersion">
              <dt class="text-zinc-500">DAT version</dt>
              <dd class="text-zinc-300">{{ verifyResult.datVersion }}</dd>
            </template>
            <template v-for="ext in visibleExternalIds(verifyResult)" :key="ext.provider">
              <dt class="text-zinc-500">{{ ext.provider }}</dt>
              <dd class="text-zinc-300">{{ ext.id }}</dd>
            </template>
          </dl>
        </div>

        <div v-if="verifyResult.error" class="rounded-lg border-l-2 border-red-500 bg-red-500/5 px-4 py-3 text-sm text-red-300">
          {{ verifyResult.error }}
        </div>
      </div>
    </template>

    <template v-else-if="isBatch && batchResults.length > 0">
      <div class="mb-4">
        <DatResultList :rows="batchRows" />
      </div>
    </template>

    <div v-else class="mb-4">
      <OutputLog :command="commandLine" :result="result" :error="error" :warnings="allWarnings" />
    </div>

    <OperationCard>
      <div class="space-y-5">
        <template v-if="isBatch">
          <BatchFileList
            :items="queue"
            :running="batch.running.value"
            :progress-slots="batch.progressSlots"
            @remove="store.removeFromQueue"
            @clear="store.clearQueue"
            @reorder="handleReorder"
            @remove-selected="handleRemoveSelected"
            @retry-failed="execute"
          />

          <FileDropZone
            label="Add more ROM files"
            model-value=""
            :multiple="true"
            :filters="ROM_FILTERS"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p) }"
            @update:files="handleFiles"
          />
        </template>

        <FileDropZone
          v-else
          :model-value="input"
          label="Input ROM file"
          :multiple="true"
          :filters="ROM_FILTERS"
          :primary="true"
          @update:model-value="handleSingleFile"
          @update:files="handleFiles"
        />

        <FlagToggle
          v-model="quick"
          label="Quick verify"
          description="Trust a zip's own CRC32 for eligible cartridge images instead of extracting and hashing. Falls back automatically when that alone does not verify."
          :disabled="loading || batch.running.value"
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
          @click="execute"
          @cancel="batch.abort()"
        >
          {{ isBatch && queue.filter(i => i.status === 'pending').length > 1 ? `Verify all (${queue.filter(i => i.status === 'pending').length})` : 'Verify' }}
        </RunButton>
      </div>
    </OperationCard>
  </div>
</template>

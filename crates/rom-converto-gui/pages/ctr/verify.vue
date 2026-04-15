<script setup lang="ts">
import { storeToRefs } from "pinia";
import { useCtrVerifyStore } from "~/stores/ctr-verify";
import { invoke } from "@tauri-apps/api/core";

const store = useCtrVerifyStore();
const { input, verifyContent, result, error, loading, queue } = storeToRefs(store);
const progress = useProgress("ctr-verify");

const isBatch = computed(() => queue.value.length > 0);

const ROM_FILTERS = [{ name: '3DS ROM', extensions: ['cia', '3ds', 'cci', 'cxi', 'zcia', 'zcci', 'zcxi'] }];

type Legitimacy = "Piratelegit" | { Legit: string } | { Standard: "Encrypted" | "Decrypted" };

interface CiaResult {
  format: "Cia";
  legitimacy: Legitimacy;
  ca_cert_valid: boolean;
  tmd_signer_cert_valid: boolean;
  ticket_signer_cert_valid: boolean;
  tmd_signature_valid: boolean;
  ticket_signature_valid: boolean;
  content_hashes_valid: boolean | null;
  title_id: string;
  console_id: number;
  title_version: number;
  details: string[];
}

interface NcchPartition {
  index: number;
  name: string;
  title_id: string;
  product_code: string;
  encrypted: boolean;
  ncch_magic_valid: boolean;
  exheader_hash_valid: boolean | null;
  logo_hash_valid: boolean | null;
  exefs_hash_valid: boolean | null;
  romfs_hash_valid: boolean | null;
  details: string[];
}

interface NcsdResult {
  format: "Ncsd";
  ncsd_magic_valid: boolean;
  title_id: string;
  partition_count: number;
  partitions: NcchPartition[];
  details: string[];
}

type VerifyResult = CiaResult | NcsdResult;

const verifyResult = ref<VerifyResult | null>(null);

const batch = useBatchOperation("ctr-verify", "cmd_verify_ctr", (item) => ({
  input: item.input,
  verifyContent: verifyContent.value,
}));

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

async function execute() {
  progress.reset();
  verifyResult.value = null;
  error.value = "";
  result.value = "";

  if (isBatch.value) {
    await batch.start(queue, result);
  } else {
    loading.value = true;
    try {
      const json = await invoke<string>("cmd_verify_ctr", {
        input: input.value,
        verifyContent: verifyContent.value,
      });
      const parsed = JSON.parse(json) as VerifyResult;
      verifyResult.value = parsed;
      result.value = parsed.format === "Cia"
        ? formatLegitimacy(parsed.legitimacy)
        : `NCSD — ${parsed.partition_count} partition(s)`;
    } catch (e: any) {
      error.value = typeof e === "string" ? e : e.message || String(e);
    } finally {
      loading.value = false;
    }
  }
}

function formatLegitimacy(leg: Legitimacy): string {
  if (typeof leg === "string") return leg;
  if ("Legit" in leg) return `Legit (${leg.Legit})`;
  if ("Standard" in leg) {
    return leg.Standard === "Decrypted" ? "Standard (Decrypted)" : "Standard";
  }
  return String(leg);
}

function legitColor(leg: Legitimacy): string {
  if (typeof leg !== "string" && "Legit" in leg) return "emerald";
  if (leg === "Piratelegit") return "amber";
  return "red";
}

function partitionOk(part: NcchPartition): boolean {
  if (!part.ncch_magic_valid) return false;
  if (part.encrypted) return true; // can't verify hashes, not an error
  return [part.exheader_hash_valid, part.exefs_hash_valid, part.romfs_hash_valid, part.logo_hash_valid]
    .every(v => v === null || v === true);
}
</script>

<template>
  <div>
    <PageHeader
      title="Verify 3DS ROM"
      description="Verify .cia legitimacy or .3ds/.cci integrity. Supports compressed Z3DS files. Drop multiple files for batch processing."
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
            label="Add more ROM files"
            model-value=""
            :multiple="true"
            :filters="ROM_FILTERS"
            @update:model-value="(p: string) => { if (p) store.addToQueue(p) }"
            @update:files="handleFiles"
          />
        </template>

        <!-- Single mode -->
        <div v-else class="grid gap-5 lg:grid-cols-2">
          <FileDropZone
            :model-value="input"
            label="Input ROM File"
            :multiple="true"
            :filters="ROM_FILTERS"
            :primary="true"
            @update:model-value="handleSingleFile"
            @update:files="handleFiles"
          />

          <div class="space-y-5">
            <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
              <FlagToggle
                v-model="verifyContent"
                label="Verify Content Hashes"
                description="Also check SHA-256 hashes of all content data (CIA only, slower)"
              />
            </div>
          </div>
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
          {{ isBatch ? `Verify ${queue.filter(i => i.status === 'pending').length} Files` : 'Verify' }}
        </RunButton>
      </div>
    </OperationCard>

    <!-- CIA result -->
    <template v-if="verifyResult && verifyResult.format === 'Cia'">
      <div class="mt-4 space-y-3">
        <!-- Legitimacy badge -->
        <div
          class="flex items-center gap-3 rounded-lg border-l-2 px-4 py-3"
          :class="{
            'border-emerald-500 bg-emerald-500/5': legitColor((verifyResult as CiaResult).legitimacy) === 'emerald',
            'border-amber-500 bg-amber-500/5': legitColor((verifyResult as CiaResult).legitimacy) === 'amber',
            'border-red-500 bg-red-500/5': legitColor((verifyResult as CiaResult).legitimacy) === 'red',
          }"
        >
          <span
            class="inline-flex items-center rounded-md px-2.5 py-1 text-sm font-semibold"
            :class="{
              'bg-emerald-500/20 text-emerald-300': legitColor((verifyResult as CiaResult).legitimacy) === 'emerald',
              'bg-amber-500/20 text-amber-300': legitColor((verifyResult as CiaResult).legitimacy) === 'amber',
              'bg-red-500/20 text-red-300': legitColor((verifyResult as CiaResult).legitimacy) === 'red',
            }"
          >
            {{ formatLegitimacy((verifyResult as CiaResult).legitimacy) }}
          </span>
          <span class="text-sm text-zinc-400">
            Title ID: {{ (verifyResult as CiaResult).title_id }} &middot; Version: {{ (verifyResult as CiaResult).title_version }}
          </span>
        </div>

        <!-- Signature checks -->
        <div class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3">
          <h4 class="mb-2 text-xs font-semibold uppercase tracking-wider text-zinc-500">Signature Checks</h4>
          <div class="space-y-1.5">
            <CheckRow label="CA Certificate" :valid="(verifyResult as CiaResult).ca_cert_valid" />
            <CheckRow label="TMD Signer (CP)" :valid="(verifyResult as CiaResult).tmd_signer_cert_valid" />
            <CheckRow label="Ticket Signer (XS)" :valid="(verifyResult as CiaResult).ticket_signer_cert_valid" />
            <CheckRow label="TMD Signature" :valid="(verifyResult as CiaResult).tmd_signature_valid" />
            <CheckRow label="Ticket Signature" :valid="(verifyResult as CiaResult).ticket_signature_valid" />
            <CheckRow
              v-if="(verifyResult as CiaResult).content_hashes_valid !== null"
              label="Content Hashes"
              :valid="(verifyResult as CiaResult).content_hashes_valid!"
            />
          </div>
        </div>

        <!-- Details -->
        <details class="rounded-lg border border-zinc-800/50 bg-zinc-800/20">
          <summary class="cursor-pointer px-4 py-2.5 text-xs font-semibold uppercase tracking-wider text-zinc-500 hover:text-zinc-400">
            Details
          </summary>
          <div class="border-t border-zinc-800/50 px-4 py-3">
            <ul class="space-y-0.5 font-mono text-xs text-zinc-400">
              <li v-for="(line, i) in (verifyResult as CiaResult).details" :key="i">{{ line }}</li>
            </ul>
          </div>
        </details>
      </div>
    </template>

    <!-- NCSD result -->
    <template v-else-if="verifyResult && verifyResult.format === 'Ncsd'">
      <div class="mt-4 space-y-3">
        <!-- Header info -->
        <div
          class="flex items-center gap-3 rounded-lg border-l-2 px-4 py-3"
          :class="(verifyResult as NcsdResult).ncsd_magic_valid ? 'border-emerald-500 bg-emerald-500/5' : 'border-red-500 bg-red-500/5'"
        >
          <span
            class="inline-flex items-center rounded-md px-2.5 py-1 text-sm font-semibold"
            :class="(verifyResult as NcsdResult).ncsd_magic_valid ? 'bg-emerald-500/20 text-emerald-300' : 'bg-red-500/20 text-red-300'"
          >
            NCSD {{ (verifyResult as NcsdResult).ncsd_magic_valid ? 'Valid' : 'Invalid' }}
          </span>
          <span class="text-sm text-zinc-400">
            Title ID: {{ (verifyResult as NcsdResult).title_id }} &middot; {{ (verifyResult as NcsdResult).partition_count }} partition(s)
          </span>
        </div>

        <!-- Per-partition results -->
        <div
          v-for="part in (verifyResult as NcsdResult).partitions"
          :key="part.index"
          class="rounded-lg border border-zinc-800/50 bg-zinc-800/20 px-4 py-3"
        >
          <div class="mb-2 flex items-center gap-2">
            <h4 class="text-xs font-semibold uppercase tracking-wider text-zinc-500">
              Partition {{ part.index }} — {{ part.name }}
            </h4>
            <span
              v-if="part.encrypted"
              class="rounded bg-amber-500/20 px-1.5 py-0.5 text-[10px] font-medium text-amber-300"
            >Encrypted</span>
            <span
              v-else
              class="rounded bg-zinc-700/50 px-1.5 py-0.5 text-[10px] font-medium text-zinc-400"
            >Decrypted</span>
            <span
              class="ml-auto rounded px-1.5 py-0.5 text-[10px] font-medium"
              :class="partitionOk(part) ? 'bg-emerald-500/20 text-emerald-300' : 'bg-red-500/20 text-red-300'"
            >{{ partitionOk(part) ? 'OK' : 'Issues' }}</span>
          </div>

          <div v-if="part.product_code" class="mb-2 text-xs text-zinc-500">
            {{ part.product_code }} &middot; {{ part.title_id }}
          </div>

          <div v-if="!part.encrypted" class="space-y-1.5">
            <CheckRow label="NCCH Magic" :valid="part.ncch_magic_valid" />
            <CheckRow v-if="part.exheader_hash_valid !== null" label="ExHeader Hash" :valid="part.exheader_hash_valid" />
            <CheckRow v-if="part.exefs_hash_valid !== null" label="ExeFS Hash" :valid="part.exefs_hash_valid" />
            <CheckRow v-if="part.romfs_hash_valid !== null" label="RomFS Hash" :valid="part.romfs_hash_valid" />
            <CheckRow v-if="part.logo_hash_valid !== null" label="Logo Hash" :valid="part.logo_hash_valid" />
          </div>
          <div v-else class="text-xs text-zinc-500">
            Hash verification skipped — content is encrypted
          </div>
        </div>

        <!-- Details -->
        <details class="rounded-lg border border-zinc-800/50 bg-zinc-800/20">
          <summary class="cursor-pointer px-4 py-2.5 text-xs font-semibold uppercase tracking-wider text-zinc-500 hover:text-zinc-400">
            Details
          </summary>
          <div class="border-t border-zinc-800/50 px-4 py-3">
            <ul class="space-y-0.5 font-mono text-xs text-zinc-400">
              <li v-for="(line, i) in (verifyResult as NcsdResult).details" :key="i">{{ line }}</li>
            </ul>
          </div>
        </details>
      </div>
    </template>

    <!-- Batch/error result (no structured data) -->
    <div v-else class="mt-4">
      <OutputLog :result="result" :error="error" />
    </div>
  </div>
</template>

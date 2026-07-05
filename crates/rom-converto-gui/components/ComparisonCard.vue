<script setup lang="ts">
import type { ComparisonSummary } from "~/types/report";

const props = defineProps<{
  comparison: ComparisonSummary;
}>();

function formatBytes(n: number): string {
  if (n < 1024) return `${n} bytes`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let value = n / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(2)} ${units[unit]}`;
}

const ratioLabel = computed(() => {
  const pct = props.comparison.ratio_pct;
  if (pct === null) return "";
  return pct >= 0 ? `${pct.toFixed(1)}% saved` : `${Math.abs(pct).toFixed(1)}% grew`;
});
</script>

<template>
  <div class="space-y-3 rounded-lg border border-zinc-800/50 bg-zinc-800/20 p-4">
    <div class="flex items-center gap-2 text-sm font-medium">
      <span class="rounded bg-zinc-700/50 px-1.5 py-0.5 text-xs uppercase tracking-wide text-zinc-300">
        {{ comparison.input_format }}
      </span>
      <svg class="h-3.5 w-3.5 shrink-0 text-zinc-500" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2" aria-hidden="true">
        <path stroke-linecap="round" stroke-linejoin="round" d="M13.5 4.5L21 12m0 0l-7.5 7.5M21 12H3" />
      </svg>
      <span class="rounded bg-zinc-700/50 px-1.5 py-0.5 text-xs uppercase tracking-wide text-zinc-300">
        {{ comparison.output_format }}
      </span>
    </div>

    <dl class="grid grid-cols-2 gap-x-4 gap-y-1 text-sm">
      <dt class="text-zinc-500">Input</dt>
      <dd class="text-zinc-200">{{ formatBytes(comparison.input_bytes) }}</dd>
      <dt class="text-zinc-500">Output</dt>
      <dd class="text-zinc-200">{{ formatBytes(comparison.output_bytes) }}</dd>
      <template v-if="ratioLabel">
        <dt class="text-zinc-500">Change</dt>
        <dd :class="comparison.ratio_pct! >= 0 ? 'text-emerald-400' : 'text-amber-400'">{{ ratioLabel }}</dd>
      </template>
      <template v-if="comparison.output_sha1">
        <dt class="text-zinc-500">SHA-1</dt>
        <dd class="truncate font-mono text-xs text-zinc-300" :title="comparison.output_sha1">
          {{ comparison.output_sha1 }}
        </dd>
      </template>
    </dl>

    <div v-if="comparison.verify" class="flex flex-wrap items-center gap-2 text-xs">
      <span
        class="rounded px-1.5 py-0.5 font-medium"
        :class="comparison.verify.ok ? 'bg-emerald-500/20 text-emerald-300' : 'bg-red-500/20 text-red-300'"
      >
        {{ comparison.verify.ok ? 'Verified' : 'Failed' }}
      </span>
      <span class="text-zinc-500">{{ comparison.verify.message }}</span>
      <span v-if="comparison.verify.round_trip" class="text-zinc-500">(round-trip checked)</span>
    </div>
  </div>
</template>

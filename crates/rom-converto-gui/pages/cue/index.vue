<script setup lang="ts">
const operations = [
  {
    to: "/cue/merge",
    title: "Merge multi-bin",
    description: "Merge a multi-track .cue with one .bin per track into a single .bin + .cue pair",
    formats: ".bin + .cue",
    icon: "compress",
  },
];
</script>

<template>
  <div class="py-6">
    <div class="mb-8">
      <NuxtLink
        to="/"
        class="inline-flex items-center gap-1 text-[11px] font-medium text-zinc-500 transition hover:text-sky-400"
      >
        <svg class="h-3 w-3" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
          <path stroke-linecap="round" stroke-linejoin="round" d="M15.75 19.5L8.25 12l7.5-7.5" />
        </svg>
        All platforms
      </NuxtLink>
      <h1 class="mt-2 text-2xl font-bold text-zinc-100">CD (CUE/BIN)</h1>
      <p class="mt-1 text-sm text-zinc-400">CUE/BIN disc image operations.</p>
    </div>

    <OperationGrid :operations="operations" />
  </div>
</template>

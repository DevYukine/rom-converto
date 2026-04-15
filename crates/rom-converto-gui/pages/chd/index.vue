<script setup lang="ts">
const operations = [
  {
    to: "/chd/compress",
    title: "Compress Disc",
    description: "Compress .bin + .cue pairs to .chd",
    formats: ".bin + .cue",
    icon: "disc-down",
  },
  {
    to: "/chd/extract",
    title: "Extract Disc",
    description: "Extract .chd back to .bin + .cue",
    formats: ".chd",
    icon: "disc-up",
  },
  {
    to: "/chd/verify",
    title: "Verify Disc",
    description: "Verify .chd integrity via SHA-1 checksums",
    formats: ".chd",
    icon: "shield-check",
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
      <h1 class="mt-2 text-2xl font-bold text-zinc-100">CD (CHD)</h1>
      <p class="mt-1 text-sm text-zinc-400">CD disc image operations.</p>
    </div>

    <OperationGrid :operations="operations" />
  </div>
</template>

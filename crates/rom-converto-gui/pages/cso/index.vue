<script setup lang="ts">
const operations = [
  {
    to: "/cso/compress",
    title: "Compress to CSO/ZSO",
    description: "Compress PSP/PS2 ISOs to .cso (PSP, PPSSPP) or .zso (PS2 via OPL)",
    formats: ".iso",
    icon: "disc-down",
  },
  {
    to: "/cso/decompress",
    title: "Decompress CSO/ZSO",
    description: "Restore the original .iso from a .cso or .zso container",
    formats: ".cso, .zso",
    icon: "disc-up",
  },
  {
    to: "/cso/verify",
    title: "Verify CSO/ZSO",
    description: "Validate the index structure and optionally decode every block",
    formats: ".cso, .zso",
    icon: "shield-check",
  },
  {
    to: "/cso/info",
    title: "CSO/ZSO info",
    description: "Read container info: format, block geometry, raw block count, and compression ratio",
    formats: ".cso, .zso",
    icon: "info",
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
      <h1 class="mt-2 text-2xl font-bold text-zinc-100">PSP / PS2 (CSO/ZSO)</h1>
      <p class="mt-1 text-sm text-zinc-400">Block-compressed ISO containers for real hardware.</p>
    </div>

    <OperationGrid :operations="operations" />
  </div>
</template>

<script setup lang="ts">
const platforms = [
  {
    to: "/ctr",
    name: "3DS",
    description: "CDN to CIA, decrypt, compress, decompress, verify, convert, info",
    count: 7,
  },
  {
    to: "/dol",
    name: "GameCube",
    description: "Compress and decompress ISO/GCM discs; read disc info",
    count: 3,
  },
  {
    to: "/rvl",
    name: "Wii",
    description: "Compress and decompress ISO/WBFS discs; read disc info",
    count: 3,
  },
  {
    to: "/wup",
    name: "Wii U",
    description: "Bundle titles into .wua, decrypt NUS, read title info",
    count: 3,
  },
  {
    to: "/nx",
    name: "Switch",
    description: "Compress NSP/XCI to NSZ/XCZ, verify integrity, read title info",
    count: 4,
  },
  {
    to: "/chd",
    name: "CD (CHD)",
    description: "Compress, extract, verify CD disc images, read info",
    count: 4,
  },
  {
    to: "/cue",
    name: "CD (CUE/BIN)",
    description: "Merge multi-bin disc images into a single bin/cue pair",
    count: 1,
  },
];
</script>

<template>
  <div class="py-8">
    <div class="mb-10 text-center">
      <h1 class="text-2xl font-bold text-zinc-100">rom-converto</h1>
      <p class="mt-2 text-sm text-zinc-400">
        Pick a platform to see the available operations.
      </p>
    </div>

    <div class="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-4">
      <NuxtLink
        v-for="p in platforms"
        :key="p.to"
        :to="p.to"
        class="group rounded-xl border border-zinc-800 bg-zinc-800/20 p-6 transition hover:border-zinc-700 hover:bg-zinc-800/40"
      >
        <div class="flex items-center justify-between gap-3">
          <h3 class="truncate text-lg font-semibold text-zinc-100">{{ p.name }}</h3>
          <span
            class="shrink-0 whitespace-nowrap rounded-full bg-sky-500/10 px-2 py-0.5 text-[11px] font-medium text-sky-400"
          >{{ p.count }} {{ p.count === 1 ? "action" : "actions" }}</span>
        </div>
        <p class="mt-3 text-xs text-zinc-500">{{ p.description }}</p>
        <div class="mt-4 flex items-center gap-1 text-[11px] font-medium text-zinc-600 transition group-hover:text-sky-400">
          <span>Open</span>
          <svg class="h-3 w-3" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
            <path stroke-linecap="round" stroke-linejoin="round" d="M8.25 4.5l7.5 7.5-7.5 7.5" />
          </svg>
        </div>
      </NuxtLink>
    </div>
  </div>
</template>

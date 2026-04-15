<script setup lang="ts">
const operations = [
  {
    to: "/ctr/cdn-to-cia",
    title: "CDN to CIA",
    description: "Convert Nintendo CDN content directories to CIA",
    formats: "CDN directories",
    icon: "folder-arrow",
  },
  {
    to: "/ctr/decrypt",
    title: "Decrypt ROM",
    description: "Decrypt encrypted 3DS ROM files for emulator use",
    formats: ".cia, .3ds, .cci, .cxi",
    icon: "lock-open",
  },
  {
    to: "/ctr/compress",
    title: "Compress ROM",
    description: "Compress decrypted ROMs to Z3DS (seekable zstd)",
    formats: ".cia, .3ds, .cci, .cxi, .3dsx",
    icon: "compress",
  },
  {
    to: "/ctr/decompress",
    title: "Decompress ROM",
    description: "Decompress Z3DS files back to the original format",
    formats: ".zcia, .zcci, .zcxi, .z3dsx",
    icon: "expand",
  },
  {
    to: "/ctr/verify",
    title: "Verify ROM",
    description: "Verify .cia legitimacy or .3ds / .cci NCCH integrity",
    formats: ".cia, .3ds, .cci (+ Z3DS)",
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
      <h1 class="mt-2 text-2xl font-bold text-zinc-100">3DS</h1>
      <p class="mt-1 text-sm text-zinc-400">Nintendo 3DS ROM operations.</p>
    </div>

    <OperationGrid :operations="operations" />
  </div>
</template>

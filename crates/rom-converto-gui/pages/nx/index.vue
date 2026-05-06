<script setup lang="ts">
const operations = [
  {
    to: "/nx/compress",
    title: "Compress to NSZ/XCZ",
    description: "Compress NSP or XCI to nsz-compatible NSZ/XCZ via per-NCA zstd",
    formats: "NSP / XCI",
    icon: "compress",
  },
  {
    to: "/nx/decompress",
    title: "Decompress to NSP/XCI",
    description: "Restore NSZ or XCZ back to a stock NSP / XCI",
    formats: "NSZ / XCZ",
    icon: "expand",
  },
  {
    to: "/nx/verify",
    title: "Verify integrity",
    description: "Decrypt every NCA section and recompute the stored hashes",
    formats: "NSP / NSZ / XCI / XCZ",
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
      <h1 class="mt-2 text-2xl font-bold text-zinc-100">Switch</h1>
      <p class="mt-1 text-sm text-zinc-400">
        NSP / XCI compression and verification. Requires a `prod.keys`
        file at $HOME/.switch/prod.keys, %USERPROFILE%\.switch\prod.keys,
        or supplied via the keys field on each page.
      </p>
    </div>

    <OperationGrid :operations="operations" />
  </div>
</template>

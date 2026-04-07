<script setup lang="ts">
const ctrLinks = [
  { to: "/ctr/cdn-to-cia", label: "CDN to CIA" },
  { to: "/ctr/decrypt", label: "Decrypt" },
  { to: "/ctr/compress", label: "Compress" },
  { to: "/ctr/decompress", label: "Decompress" },
];

const chdLinks = [
  { to: "/chd/compress", label: "Compress" },
  { to: "/chd/extract", label: "Extract" },
  { to: "/chd/verify", label: "Verify" },
];
</script>

<template>
  <div class="flex h-screen bg-zinc-900 text-zinc-100">
    <!-- Full-screen drop overlay -->
    <Transition
      enter-active-class="transition duration-150"
      enter-from-class="opacity-0"
      enter-to-class="opacity-100"
      leave-active-class="transition duration-150"
      leave-from-class="opacity-100"
      leave-to-class="opacity-0"
    >
      <div
        v-if="isDraggingOver"
        class="pointer-events-none fixed inset-0 z-50 flex items-center justify-center bg-sky-500/10 backdrop-blur-sm"
      >
        <div class="rounded-2xl border-2 border-dashed border-sky-400 bg-zinc-900/80 px-12 py-8 text-center">
          <p class="text-lg font-semibold text-sky-400">Drop file here</p>
          <p class="mt-1 text-sm text-zinc-400">
            Release to load the file
          </p>
        </div>
      </div>
    </Transition>

    <!-- Sidebar -->
    <nav class="flex w-56 flex-col border-r border-zinc-800 bg-zinc-950 p-4">
      <NuxtLink
        to="/"
        class="mb-6 text-lg font-bold tracking-tight text-sky-400"
      >
        rom-converto
      </NuxtLink>

      <div class="space-y-5">
        <div>
          <h3 class="mb-1.5 text-xs font-semibold uppercase tracking-wider text-zinc-500">
            CTR (3DS)
          </h3>
          <ul class="space-y-0.5">
            <li v-for="link in ctrLinks" :key="link.to">
              <NuxtLink
                :to="link.to"
                class="block rounded-md px-3 py-1.5 text-sm transition hover:bg-zinc-800"
                active-class="bg-zinc-800 text-sky-400"
              >
                {{ link.label }}
              </NuxtLink>
            </li>
          </ul>
        </div>

        <div>
          <h3 class="mb-1.5 text-xs font-semibold uppercase tracking-wider text-zinc-500">
            CHD
          </h3>
          <ul class="space-y-0.5">
            <li v-for="link in chdLinks" :key="link.to">
              <NuxtLink
                :to="link.to"
                class="block rounded-md px-3 py-1.5 text-sm transition hover:bg-zinc-800"
                active-class="bg-zinc-800 text-sky-400"
              >
                {{ link.label }}
              </NuxtLink>
            </li>
          </ul>
        </div>
      </div>

      <div class="mt-auto text-xs text-zinc-600">v0.4.0</div>
    </nav>

    <!-- Main content -->
    <main class="flex-1 overflow-y-auto p-8">
      <slot />
    </main>
  </div>
</template>

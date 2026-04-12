<script setup lang="ts">
import { useCtrCdnToCiaStore } from "~/stores/ctr-cdn-to-cia";
import { useCtrDecryptStore } from "~/stores/ctr-decrypt";
import { useCtrCompressStore } from "~/stores/ctr-compress";
import { useCtrDecompressStore } from "~/stores/ctr-decompress";
import { useCtrVerifyStore } from "~/stores/ctr-verify";
import { useChdCompressStore } from "~/stores/chd-compress";
import { useChdExtractStore } from "~/stores/chd-extract";
import { useChdVerifyStore } from "~/stores/chd-verify";
import { getVersion } from "@tauri-apps/api/app";

const appVersion = ref("");
getVersion().then((v) => { appVersion.value = v; }).catch(() => {});

const ctrLinks = [
  { to: "/ctr/cdn-to-cia", label: "CDN to CIA", store: () => useCtrCdnToCiaStore(), icon: "folder-arrow" },
  { to: "/ctr/decrypt", label: "Decrypt", store: () => useCtrDecryptStore(), icon: "lock-open" },
  { to: "/ctr/compress", label: "Compress", store: () => useCtrCompressStore(), icon: "compress" },
  { to: "/ctr/decompress", label: "Decompress", store: () => useCtrDecompressStore(), icon: "expand" },
  { to: "/ctr/verify", label: "Verify", store: () => useCtrVerifyStore(), icon: "shield-check" },
];

const chdLinks = [
  { to: "/chd/compress", label: "Compress", store: () => useChdCompressStore(), icon: "disc-down" },
  { to: "/chd/extract", label: "Extract", store: () => useChdExtractStore(), icon: "disc-up" },
  { to: "/chd/verify", label: "Verify", store: () => useChdVerifyStore(), icon: "shield-check" },
];

function getStatus(store: () => { loading: boolean; result: string; error: string }) {
  const s = store();
  if (s.loading) return "running";
  if (s.error) return "error";
  if (s.result) return "done";
  return "idle";
}
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
        class="pointer-events-none fixed inset-0 z-50 flex items-center justify-center bg-zinc-900/60 backdrop-blur-md"
      >
        <div class="flex flex-col items-center gap-3 rounded-2xl border-2 border-dashed border-sky-400/60 bg-zinc-900/90 px-16 py-10">
          <svg class="h-12 w-12 text-sky-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
            <path stroke-linecap="round" stroke-linejoin="round" d="M3 16.5v2.25A2.25 2.25 0 005.25 21h13.5A2.25 2.25 0 0021 18.75V16.5m-13.5-9L12 3m0 0l4.5 4.5M12 3v13.5" />
          </svg>
          <p class="text-lg font-semibold text-sky-400">Drop files here</p>
          <p class="text-sm text-zinc-400">Release to load</p>
        </div>
      </div>
    </Transition>

    <!-- Sidebar -->
    <nav class="flex w-60 flex-col border-r border-zinc-800/50 bg-zinc-950">
      <!-- Brand -->
      <div class="px-5 py-5">
        <NuxtLink to="/" class="flex items-center gap-2.5">
          <div class="flex h-8 w-8 items-center justify-center rounded-lg bg-sky-500/10">
            <svg class="h-4 w-4 text-sky-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
              <path stroke-linecap="round" stroke-linejoin="round" d="M19.5 14.25v-2.625a3.375 3.375 0 00-3.375-3.375h-1.5A1.125 1.125 0 0113.5 7.125v-1.5a3.375 3.375 0 00-3.375-3.375H8.25m.75 12l3 3m0 0l3-3m-3 3v-6m-1.5-9H5.625c-.621 0-1.125.504-1.125 1.125v17.25c0 .621.504 1.125 1.125 1.125h12.75c.621 0 1.125-.504 1.125-1.125V11.25a9 9 0 00-9-9z" />
            </svg>
          </div>
          <span class="text-sm font-bold tracking-tight text-zinc-100">rom-converto</span>
        </NuxtLink>
      </div>

      <!-- Navigation -->
      <div class="flex-1 space-y-6 px-3 pt-2">
        <!-- CTR Section -->
        <div>
          <h3 class="mb-2 px-2 text-[11px] font-semibold uppercase tracking-wider text-zinc-500">
            CTR (3DS)
          </h3>
          <ul class="space-y-0.5">
            <li v-for="link in ctrLinks" :key="link.to">
              <NuxtLink
                :to="link.to"
                class="group relative flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-sm text-zinc-400 transition hover:bg-zinc-800/60 hover:text-zinc-200"
                active-class="!bg-zinc-800 !text-sky-400 before:absolute before:left-0 before:top-1.5 before:bottom-1.5 before:w-0.5 before:rounded-full before:bg-sky-400"
              >
                <!-- Icons -->
                <span class="flex h-5 w-5 items-center justify-center">
                  <!-- folder-arrow -->
                  <svg v-if="link.icon === 'folder-arrow'" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
                    <path stroke-linecap="round" stroke-linejoin="round" d="M3.75 9.776c.112-.017.227-.026.344-.026h15.812c.117 0 .232.009.344.026m-16.5 0a2.25 2.25 0 00-1.883 2.542l.857 6a2.25 2.25 0 002.227 1.932H19.05a2.25 2.25 0 002.227-1.932l.857-6a2.25 2.25 0 00-1.883-2.542m-16.5 0V6A2.25 2.25 0 016 3.75h3.879a1.5 1.5 0 011.06.44l2.122 2.12a1.5 1.5 0 001.06.44H18A2.25 2.25 0 0120.25 9v.776" />
                  </svg>
                  <!-- lock-open -->
                  <svg v-else-if="link.icon === 'lock-open'" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
                    <path stroke-linecap="round" stroke-linejoin="round" d="M13.5 10.5V6.75a4.5 4.5 0 119 0v3.75M3.75 21.75h10.5a2.25 2.25 0 002.25-2.25v-6.75a2.25 2.25 0 00-2.25-2.25H3.75a2.25 2.25 0 00-2.25 2.25v6.75a2.25 2.25 0 002.25 2.25z" />
                  </svg>
                  <!-- compress -->
                  <svg v-else-if="link.icon === 'compress'" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
                    <path stroke-linecap="round" stroke-linejoin="round" d="M9 9V4.5M9 9H4.5M9 9L3.75 3.75M9 15v4.5M9 15H4.5M9 15l-5.25 5.25M15 9h4.5M15 9V4.5M15 9l5.25-5.25M15 15h4.5M15 15v4.5m0-4.5l5.25 5.25" />
                  </svg>
                  <!-- expand -->
                  <svg v-else-if="link.icon === 'expand'" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
                    <path stroke-linecap="round" stroke-linejoin="round" d="M3.75 3.75v4.5m0-4.5h4.5m-4.5 0L9 9M3.75 20.25v-4.5m0 4.5h4.5m-4.5 0L9 15M20.25 3.75h-4.5m4.5 0v4.5m0-4.5L15 9m5.25 11.25h-4.5m4.5 0v-4.5m0 4.5L15 15" />
                  </svg>
                  <!-- shield-check -->
                  <svg v-else-if="link.icon === 'shield-check'" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
                    <path stroke-linecap="round" stroke-linejoin="round" d="M9 12.75L11.25 15 15 9.75m-3-7.036A11.959 11.959 0 013.598 6 11.99 11.99 0 003 9.749c0 5.592 3.824 10.29 9 11.623 5.176-1.332 9-6.03 9-11.622 0-1.31-.21-2.571-.598-3.751h-.152c-3.196 0-6.1-1.248-8.25-3.285z" />
                  </svg>
                </span>

                {{ link.label }}

                <!-- Status dot -->
                <span class="ml-auto flex h-4 w-4 items-center justify-center">
                  <span
                    v-if="getStatus(link.store) === 'running'"
                    class="h-2 w-2 animate-pulse rounded-full bg-sky-400"
                  />
                  <span
                    v-else-if="getStatus(link.store) === 'done'"
                    class="h-1.5 w-1.5 rounded-full bg-emerald-400"
                  />
                  <span
                    v-else-if="getStatus(link.store) === 'error'"
                    class="h-1.5 w-1.5 rounded-full bg-red-400"
                  />
                </span>
              </NuxtLink>
            </li>
          </ul>
        </div>

        <!-- CHD Section -->
        <div>
          <h3 class="mb-2 px-2 text-[11px] font-semibold uppercase tracking-wider text-zinc-500">
            CHD
          </h3>
          <ul class="space-y-0.5">
            <li v-for="link in chdLinks" :key="link.to">
              <NuxtLink
                :to="link.to"
                class="group relative flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-sm text-zinc-400 transition hover:bg-zinc-800/60 hover:text-zinc-200"
                active-class="!bg-zinc-800 !text-sky-400 before:absolute before:left-0 before:top-1.5 before:bottom-1.5 before:w-0.5 before:rounded-full before:bg-sky-400"
              >
                <span class="flex h-5 w-5 items-center justify-center">
                  <!-- disc-down -->
                  <svg v-if="link.icon === 'disc-down'" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
                    <path stroke-linecap="round" stroke-linejoin="round" d="M20.25 6.375c0 2.278-3.694 4.125-8.25 4.125S3.75 8.653 3.75 6.375m16.5 0c0-2.278-3.694-4.125-8.25-4.125S3.75 4.097 3.75 6.375m16.5 0v11.25c0 2.278-3.694 4.125-8.25 4.125s-8.25-1.847-8.25-4.125V6.375m16.5 0v3.75m-16.5-3.75v3.75m16.5 0v3.75C20.25 16.153 16.556 18 12 18s-8.25-1.847-8.25-4.125v-3.75m16.5 0c0 2.278-3.694 4.125-8.25 4.125s-8.25-1.847-8.25-4.125" />
                  </svg>
                  <!-- disc-up -->
                  <svg v-else-if="link.icon === 'disc-up'" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
                    <path stroke-linecap="round" stroke-linejoin="round" d="M3 16.5v2.25A2.25 2.25 0 005.25 21h13.5A2.25 2.25 0 0021 18.75V16.5m-13.5-9L12 3m0 0l4.5 4.5M12 3v13.5" />
                  </svg>
                  <!-- shield-check -->
                  <svg v-else-if="link.icon === 'shield-check'" class="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.5">
                    <path stroke-linecap="round" stroke-linejoin="round" d="M9 12.75L11.25 15 15 9.75m-3-7.036A11.959 11.959 0 013.598 6 11.99 11.99 0 003 9.749c0 5.592 3.824 10.29 9 11.623 5.176-1.332 9-6.03 9-11.622 0-1.31-.21-2.571-.598-3.751h-.152c-3.196 0-6.1-1.248-8.25-3.285z" />
                  </svg>
                </span>

                {{ link.label }}

                <!-- Status dot -->
                <span class="ml-auto flex h-4 w-4 items-center justify-center">
                  <span
                    v-if="getStatus(link.store) === 'running'"
                    class="h-2 w-2 animate-pulse rounded-full bg-sky-400"
                  />
                  <span
                    v-else-if="getStatus(link.store) === 'done'"
                    class="h-1.5 w-1.5 rounded-full bg-emerald-400"
                  />
                  <span
                    v-else-if="getStatus(link.store) === 'error'"
                    class="h-1.5 w-1.5 rounded-full bg-red-400"
                  />
                </span>
              </NuxtLink>
            </li>
          </ul>
        </div>
      </div>

      <!-- Footer -->
      <div class="border-t border-zinc-800/50 px-5 py-3">
        <span class="text-[11px] text-zinc-600">{{ appVersion ? `v${appVersion}` : '' }}</span>
      </div>
    </nav>

    <!-- Main content -->
    <main class="flex-1 overflow-y-auto px-8 py-6 lg:px-12 xl:px-20 xl:py-10 2xl:px-32">
      <slot />
    </main>
  </div>
</template>

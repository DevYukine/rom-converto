<script setup lang="ts">
interface Operation {
  to: string;
  title: string;
  description: string;
  formats: string;
  icon: string;
}

defineProps<{ operations: Operation[] }>();
</script>

<template>
  <div class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
    <NuxtLink
      v-for="op in operations"
      :key="op.to"
      :to="op.to"
      class="group rounded-xl border border-zinc-800 bg-zinc-800/20 p-5 transition hover:border-zinc-700 hover:bg-zinc-800/40"
    >
      <div
        class="mb-3 flex h-9 w-9 items-center justify-center rounded-lg bg-sky-500/10 transition group-hover:bg-sky-500/15"
      >
        <!-- folder-arrow -->
        <svg
          v-if="op.icon === 'folder-arrow'"
          class="h-4.5 w-4.5 text-sky-400"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          stroke-width="1.5"
        >
          <path
            stroke-linecap="round"
            stroke-linejoin="round"
            d="M3.75 9.776c.112-.017.227-.026.344-.026h15.812c.117 0 .232.009.344.026m-16.5 0a2.25 2.25 0 00-1.883 2.542l.857 6a2.25 2.25 0 002.227 1.932H19.05a2.25 2.25 0 002.227-1.932l.857-6a2.25 2.25 0 00-1.883-2.542m-16.5 0V6A2.25 2.25 0 016 3.75h3.879a1.5 1.5 0 011.06.44l2.122 2.12a1.5 1.5 0 001.06.44H18A2.25 2.25 0 0120.25 9v.776"
          />
        </svg>
        <!-- lock-open -->
        <svg
          v-else-if="op.icon === 'lock-open'"
          class="h-4.5 w-4.5 text-sky-400"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          stroke-width="1.5"
        >
          <path
            stroke-linecap="round"
            stroke-linejoin="round"
            d="M13.5 10.5V6.75a4.5 4.5 0 119 0v3.75M3.75 21.75h10.5a2.25 2.25 0 002.25-2.25v-6.75a2.25 2.25 0 00-2.25-2.25H3.75a2.25 2.25 0 00-2.25 2.25v6.75a2.25 2.25 0 002.25 2.25z"
          />
        </svg>
        <!-- compress -->
        <svg
          v-else-if="op.icon === 'compress'"
          class="h-4.5 w-4.5 text-sky-400"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          stroke-width="1.5"
        >
          <path
            stroke-linecap="round"
            stroke-linejoin="round"
            d="M9 9V4.5M9 9H4.5M9 9L3.75 3.75M9 15v4.5M9 15H4.5M9 15l-5.25 5.25M15 9h4.5M15 9V4.5M15 9l5.25-5.25M15 15h4.5M15 15v4.5m0-4.5l5.25 5.25"
          />
        </svg>
        <!-- expand -->
        <svg
          v-else-if="op.icon === 'expand'"
          class="h-4.5 w-4.5 text-sky-400"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          stroke-width="1.5"
        >
          <path
            stroke-linecap="round"
            stroke-linejoin="round"
            d="M3.75 3.75v4.5m0-4.5h4.5m-4.5 0L9 9M3.75 20.25v-4.5m0 4.5h4.5m-4.5 0L9 15M20.25 3.75h-4.5m4.5 0v4.5m0-4.5L15 9m5.25 11.25h-4.5m4.5 0v-4.5m0 4.5L15 15"
          />
        </svg>
        <!-- shield-check -->
        <svg
          v-else-if="op.icon === 'shield-check'"
          class="h-4.5 w-4.5 text-sky-400"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          stroke-width="1.5"
        >
          <path
            stroke-linecap="round"
            stroke-linejoin="round"
            d="M9 12.75L11.25 15 15 9.75m-3-7.036A11.959 11.959 0 013.598 6 11.99 11.99 0 003 9.749c0 5.592 3.824 10.29 9 11.623 5.176-1.332 9-6.03 9-11.622 0-1.31-.21-2.571-.598-3.751h-.152c-3.196 0-6.1-1.248-8.25-3.285z"
          />
        </svg>
        <!-- disc-down -->
        <svg
          v-else-if="op.icon === 'disc-down'"
          class="h-4.5 w-4.5 text-sky-400"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          stroke-width="1.5"
        >
          <path
            stroke-linecap="round"
            stroke-linejoin="round"
            d="M20.25 6.375c0 2.278-3.694 4.125-8.25 4.125S3.75 8.653 3.75 6.375m16.5 0c0-2.278-3.694-4.125-8.25-4.125S3.75 4.097 3.75 6.375m16.5 0v11.25c0 2.278-3.694 4.125-8.25 4.125s-8.25-1.847-8.25-4.125V6.375m16.5 0v3.75m-16.5-3.75v3.75m16.5 0v3.75C20.25 16.153 16.556 18 12 18s-8.25-1.847-8.25-4.125v-3.75m16.5 0c0 2.278-3.694 4.125-8.25 4.125s-8.25-1.847-8.25-4.125"
          />
        </svg>
        <!-- disc-up -->
        <svg
          v-else-if="op.icon === 'disc-up'"
          class="h-4.5 w-4.5 text-sky-400"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          stroke-width="1.5"
        >
          <path
            stroke-linecap="round"
            stroke-linejoin="round"
            d="M3 16.5v2.25A2.25 2.25 0 005.25 21h13.5A2.25 2.25 0 0021 18.75V16.5m-13.5-9L12 3m0 0l4.5 4.5M12 3v13.5"
          />
        </svg>
      </div>
      <h3 class="text-sm font-semibold text-zinc-200">{{ op.title }}</h3>
      <p class="mt-1 text-xs text-zinc-500">{{ op.description }}</p>
      <p class="mt-2 text-[11px] font-medium text-zinc-600">{{ op.formats }}</p>
    </NuxtLink>
  </div>
</template>

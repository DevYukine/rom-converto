<script setup lang="ts">
defineProps<{
  result?: string;
  error?: string;
}>();

const resultCopied = ref(false);
const errorCopied = ref(false);

async function copy(text: string | undefined, which: "result" | "error") {
  if (!text) return;
  const flag = which === "result" ? resultCopied : errorCopied;
  try {
    await navigator.clipboard.writeText(text);
    flag.value = true;
    setTimeout(() => {
      flag.value = false;
    }, 1500);
  } catch {
    // clipboard may be unavailable; ignore
  }
}
</script>

<template>
  <div v-if="result || error" class="space-y-2">
    <div
      v-if="result"
      role="status"
      aria-live="polite"
      class="flex items-start gap-2.5 rounded-lg border-l-2 border-emerald-500 bg-emerald-500/5 px-4 py-3"
    >
      <svg class="mt-0.5 h-4 w-4 shrink-0 text-emerald-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
        <path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7" />
      </svg>
      <pre class="max-h-64 flex-1 overflow-auto whitespace-pre-wrap break-words font-sans text-sm text-emerald-300">{{ result }}</pre>
      <button
        type="button"
        class="shrink-0 rounded-md bg-zinc-700/50 px-3 py-1 text-xs font-medium text-zinc-300 transition hover:bg-zinc-700 hover:text-zinc-100"
        @click="copy(result, 'result')"
      >
        {{ resultCopied ? "Copied!" : "Copy" }}
      </button>
    </div>

    <div
      v-if="error"
      role="alert"
      aria-live="assertive"
      class="flex items-start gap-2.5 rounded-lg border-l-2 border-red-500 bg-red-500/5 px-4 py-3"
    >
      <svg class="mt-0.5 h-4 w-4 shrink-0 text-red-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
        <path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
      </svg>
      <pre class="max-h-64 flex-1 overflow-auto whitespace-pre-wrap break-words font-mono text-sm text-red-300">{{ error }}</pre>
      <button
        type="button"
        class="shrink-0 rounded-md bg-zinc-700/50 px-3 py-1 text-xs font-medium text-zinc-300 transition hover:bg-zinc-700 hover:text-zinc-100"
        @click="copy(error, 'error')"
      >
        {{ errorCopied ? "Copied!" : "Copy" }}
      </button>
    </div>
  </div>
</template>

<script setup lang="ts">
defineProps<{
  command?: string;
  result?: string;
  preview?: string;
  cancelled?: string;
  error?: string;
}>();

const commandCopied = ref(false);
const resultCopied = ref(false);
const previewCopied = ref(false);
const errorCopied = ref(false);

async function copy(
  text: string | undefined,
  which: "command" | "result" | "preview" | "error",
) {
  if (!text) return;
  const flag =
    which === "command"
      ? commandCopied
      : which === "result"
        ? resultCopied
        : which === "preview"
          ? previewCopied
          : errorCopied;
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
  <div v-if="command || result || preview || cancelled || error" class="space-y-2">
    <div
      v-if="command"
      class="flex items-start gap-2.5 rounded-lg border-l-2 border-zinc-600 bg-zinc-800/40 px-4 py-3"
    >
      <pre class="max-h-32 flex-1 overflow-auto whitespace-pre-wrap break-all font-mono text-sm text-zinc-400">{{ command }}</pre>
      <button
        type="button"
        class="shrink-0 rounded-md bg-zinc-700/50 px-3 py-1 text-xs font-medium text-zinc-300 transition hover:bg-zinc-700 hover:text-zinc-100"
        @click="copy(command, 'command')"
      >
        {{ commandCopied ? "Copied!" : "Copy" }}
      </button>
    </div>

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
      v-if="preview"
      role="status"
      aria-live="polite"
      class="flex items-start gap-2.5 rounded-lg border-l-2 border-sky-500 bg-sky-500/5 px-4 py-3"
    >
      <svg class="mt-0.5 h-4 w-4 shrink-0 text-sky-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
        <path stroke-linecap="round" stroke-linejoin="round" d="M21 21l-4.35-4.35M11 19a8 8 0 100-16 8 8 0 000 16z" />
      </svg>
      <pre class="max-h-64 flex-1 overflow-auto whitespace-pre-wrap break-words font-mono text-sm text-sky-300">{{ preview }}</pre>
      <button
        type="button"
        class="shrink-0 rounded-md bg-zinc-700/50 px-3 py-1 text-xs font-medium text-zinc-300 transition hover:bg-zinc-700 hover:text-zinc-100"
        @click="copy(preview, 'preview')"
      >
        {{ previewCopied ? "Copied!" : "Copy" }}
      </button>
    </div>

    <div
      v-if="cancelled"
      role="status"
      aria-live="polite"
      class="flex items-start gap-2.5 rounded-lg border-l-2 border-amber-400 bg-amber-400/5 px-4 py-3"
    >
      <svg class="mt-0.5 h-4 w-4 shrink-0 text-amber-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
        <path stroke-linecap="round" stroke-linejoin="round" d="M10 9v6m4-6v6M5 7h14l-1 13a2 2 0 01-2 2H8a2 2 0 01-2-2L5 7z" />
      </svg>
      <pre class="max-h-64 flex-1 overflow-auto whitespace-pre-wrap break-words font-sans text-sm text-amber-300">{{ cancelled }}</pre>
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

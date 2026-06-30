import { computed, type ComputedRef, type Ref } from "vue";
import type { BatchItem } from "~/types/batch";

export interface PageGatingOptions {
  input?: Ref<string>;
  queue?: Ref<BatchItem[]>;
  output?: Ref<string>;
  outputTemplate?: Ref<string>;
  requireOutput?: boolean;
  outputOrTemplate?: boolean;
  templateSingleOnly?: boolean;
  emptyInputReason?: string;
  noOutputReason?: string;
}

export interface PageGating {
  canRun: ComputedRef<boolean>;
  runBlockReason: ComputedRef<string>;
  templateActive: ComputedRef<boolean>;
}

export const OUTPUT_TEMPLATE_TOOLTIP =
  "The output template determines the output path. Clear the template to set an explicit path.";

export const WUP_RENAME_TOOLTIP = "The output is a directory, so rename is not supported here.";

export function templateIsActive(raw: string | undefined): boolean {
  return (raw ?? "").trim() !== "";
}

export function templateIsBlank(raw: string | undefined): boolean {
  return raw !== undefined && raw !== "" && raw.trim() === "";
}

const BLANK_TEMPLATE_REASON = "The output template is blank. Enter a template or clear the field.";
const OUTPUT_OR_TEMPLATE_REASON = "Choose an output file or enter an output template.";

export function usePageGating(opts: PageGatingOptions): PageGating {
  const batchActive = computed(() => (opts.queue?.value?.length ?? 0) > 0);

  const templateLive = computed(() => !(opts.templateSingleOnly && batchActive.value));

  const templateActive = computed(
    () => templateLive.value && templateIsActive(opts.outputTemplate?.value),
  );

  const hasInput = computed(() => {
    if (batchActive.value) {
      return opts.queue!.value.some((i) => i.status === "pending");
    }
    return (opts.input?.value ?? "").trim() !== "";
  });

  const runBlockReason = computed(() => {
    if (!hasInput.value) {
      return opts.emptyInputReason ?? "Add an input file or folder to continue.";
    }
    if (templateLive.value && templateIsBlank(opts.outputTemplate?.value)) {
      return BLANK_TEMPLATE_REASON;
    }
    if (opts.requireOutput && (opts.output?.value ?? "").trim() === "") {
      return opts.noOutputReason ?? "Choose an output path.";
    }
    if (
      opts.outputOrTemplate &&
      !batchActive.value &&
      (opts.output?.value ?? "").trim() === "" &&
      !templateActive.value
    ) {
      return OUTPUT_OR_TEMPLATE_REASON;
    }
    return "";
  });

  const canRun = computed(() => runBlockReason.value === "");

  return { canRun, runBlockReason, templateActive };
}

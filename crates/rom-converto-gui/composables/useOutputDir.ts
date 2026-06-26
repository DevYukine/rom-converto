import { ref } from "vue";

export function useOutputDir() {
  const outputDir = ref("");
  const resolve = (derivedPath: string) => withOutputDir(derivedPath, outputDir.value);
  return { outputDir, resolve };
}

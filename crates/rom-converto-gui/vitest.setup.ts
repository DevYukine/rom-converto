import { createPinia, setActivePinia } from "pinia";
import { computed, reactive, ref, watch } from "vue";

// Nuxt auto-imports these into every store and composable; plain vitest does not.
Object.assign(globalThis, { computed, reactive, ref, watch });

setActivePinia(createPinia());

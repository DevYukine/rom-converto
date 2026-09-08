export * from "./generated/info";
export * from "./generated/runner";
export * from "./generated/report";
export * from "./generated/config";

import type { Preset } from "./generated/config";
export type PresetFormat = keyof Preset;

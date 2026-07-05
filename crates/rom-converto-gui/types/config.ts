// Mirrors rom_converto_lib::config's UserConfig/Preset structs and their
// per-format defaults, so the GUI can read and write the same
// `rom-converto.toml` presets the CLI reads. All fields are optional: an
// unset key falls through to the CLI's own config-default/built-in
// precedence when a preset is applied via the CLI.

export interface DiscDefaults {
  level?: number | null;
  chunk_size?: number | null;
  on_conflict?: string | null;
  output_dir?: string | null;
  report?: string | null;
}

export interface NxDefaults {
  level?: number | null;
  mode?: string | null;
  block_size_exp?: number | null;
  on_conflict?: string | null;
  output_dir?: string | null;
  report?: string | null;
}

export interface ChdDefaults {
  hunk_size?: number | null;
  on_conflict?: string | null;
  output_dir?: string | null;
  report?: string | null;
}

export interface CsoDefaults {
  block_size?: number | null;
  on_conflict?: string | null;
  output_dir?: string | null;
  report?: string | null;
}

export interface WupDefaults {
  level?: number | null;
  on_conflict?: string | null;
}

export interface DatDefaults {
  api_base?: string | null;
  report?: string | null;
  input_checksum_min?: string | null;
  input_checksum_max?: string | null;
}

export interface Preset {
  dol?: DiscDefaults | null;
  rvl?: DiscDefaults | null;
  nx?: NxDefaults | null;
  chd?: ChdDefaults | null;
  cso?: CsoDefaults | null;
  wup?: WupDefaults | null;
  dat?: DatDefaults | null;
}

export type PresetFormat = keyof Preset;

export interface UserConfig extends Preset {
  presets: Record<string, Preset>;
}

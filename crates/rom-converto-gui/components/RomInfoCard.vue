<script setup lang="ts">
import { computed, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import {
  type InfoResult,
  imageToDataUrl,
  nxTitleKindDisplayName,
  pickIconImage,
} from "~/types/info";

const props = defineProps<{ info: InfoResult }>();

const iconUrl = computed(() => {
  const img = pickIconImage(props.info);
  return img ? imageToDataUrl(img) : null;
});

const title = computed(() => {
  switch (props.info.kind) {
    case "ctr": {
      const t = props.info.smdh?.titles?.[0];
      return t?.long_description || props.info.product_code || props.info.title_id;
    }
    case "dol":
      return props.info.game_name || props.info.game_id;
    case "rvl":
      return props.info.game_name || props.info.game_id;
    case "wup": {
      const longs = props.info.meta?.long_names?.entries ?? [];
      return longs[0]?.[1] || props.info.title_id_hex;
    }
    case "nx": {
      const t = props.info.full?.control?.titles?.[0];
      return t?.name || props.info.full?.application_title_id?.toString() || "Switch container";
    }
    case "chd":
      return `CHD v${props.info.version}`;
    case "cso":
      return `${props.info.format} v${props.info.version}`;
    default:
      return "Unknown";
  }
});

const subtitle = computed(() => {
  switch (props.info.kind) {
    case "ctr":
      return props.info.smdh?.titles?.[0]?.publisher ?? "";
    case "dol":
      return props.info.banner?.titles?.[0]?.long_maker ?? "";
    case "rvl":
      return props.info.tmd ? `TMD ${props.info.tmd.region_name}` : "";
    case "wup": {
      const pubs = props.info.meta?.publishers?.entries ?? [];
      return pubs[0]?.[1] ?? "";
    }
    case "nx":
      return props.info.full?.control?.titles?.[0]?.publisher ?? "";
    case "chd":
      return props.info.compressors.join(", ");
    case "cso":
      return "";
  }
});

interface Field {
  label: string;
  value: string;
}

interface LocalizedEntry {
  language: string;
  name: string;
  publisher: string;
}

function languageLabel(code: string): string {
  return code
    .split("_")
    .map((p) => (p.length === 0 ? "" : p[0]!.toUpperCase() + p.slice(1)))
    .join(" ");
}

function formatMaker(code: string, name: string | null): string {
  if (name && name.length > 0) {
    return `${code} (${name})`;
  }
  return code;
}

function formatBytes(n: number): string {
  if (n < 1024) {
    return `${n} bytes`;
  }
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let value = n / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(2)} ${units[unit]} (${n.toLocaleString()} bytes)`;
}

function formatBool(value: boolean, qualifier?: string): string {
  const label = value ? "Yes" : "No";
  return qualifier ? `${label} (${qualifier})` : label;
}

function nxLabel(snake: string): string {
  return snake
    .split("_")
    .map((p) => {
      if (p === "cdn") return "CDN";
      if (p.length === 0) return "";
      return p[0]!.toUpperCase() + p.slice(1);
    })
    .join(" ");
}

const fields = computed<Field[]>(() => {
  const f: Field[] = [];
  switch (props.info.kind) {
    case "ctr": {
      f.push({ label: "Title ID", value: props.info.title_id });
      f.push({ label: "Program ID", value: props.info.program_id });
      f.push({ label: "Product code", value: props.info.product_code });
      f.push({ label: "Maker", value: formatMaker(props.info.maker_code, props.info.maker_name) });
      f.push({
        label: "Encrypted",
        value: formatBool(props.info.ncch_encrypted),
      });
      if (props.info.compressed) {
        f.push({ label: "Compressed", value: "yes (zstd)" });
      }
      if (props.info.smdh?.region_names?.length) {
        f.push({ label: "Region", value: props.info.smdh.region_names.join(", ") });
      }
      break;
    }
    case "dol": {
      f.push({ label: "Game ID", value: props.info.game_id });
      f.push({ label: "Maker", value: formatMaker(props.info.maker_code, props.info.maker_name) });
      f.push({ label: "Region", value: props.info.region });
      f.push({ label: "Disc number", value: String(props.info.disc_number) });
      f.push({ label: "Disc version", value: String(props.info.disc_version) });
      break;
    }
    case "rvl": {
      f.push({ label: "Game ID", value: props.info.game_id });
      f.push({ label: "Maker", value: formatMaker(props.info.maker_code, props.info.maker_name) });
      f.push({ label: "Region", value: props.info.region });
      if (props.info.tmd) {
        f.push({
          label: "Title ID",
          value: props.info.tmd.title_id.toString(16).padStart(16, "0").toUpperCase(),
        });
        f.push({ label: "Title version", value: String(props.info.tmd.title_version) });
        if (props.info.tmd.ios_slot !== null) {
          f.push({ label: "IOS slot", value: `IOS${props.info.tmd.ios_slot}` });
        }
      }
      break;
    }
    case "wup": {
      f.push({ label: "Title ID", value: props.info.title_id_hex });
      f.push({ label: "Title type", value: props.info.title_type });
      if (props.info.update_version !== null && props.info.update_version !== undefined) {
        f.push({
          label: "Title version",
          value: `v${props.info.update_version} (base v${props.info.title_version})`,
        });
      } else {
        f.push({ label: "Title version", value: `v${props.info.title_version}` });
      }
      if (props.info.bundled_titles && props.info.bundled_titles.length > 1) {
        const kinds = props.info.bundled_titles.map((b) => b.title_type);
        const hasUpdate = kinds.includes("Update");
        const hasDlc = kinds.includes("DLC");
        const tags: string[] = [];
        if (hasUpdate) tags.push("Update");
        if (hasDlc) tags.push("DLC");
        if (tags.length) {
          f.push({ label: "Includes", value: tags.join(" + ") });
        }
      }
      if (props.info.meta?.product_code) {
        f.push({ label: "Product code", value: props.info.meta.product_code });
      }
      if (props.info.meta?.company_code) {
        f.push({
          label: "Company",
          value: formatMaker(props.info.meta.company_code, props.info.meta.company_name ?? null),
        });
      }
      if (props.info.meta?.region_names?.length) {
        f.push({ label: "Region", value: props.info.meta.region_names.join(", ") });
      }
      if (props.info.meta?.mastering_date) {
        f.push({ label: "Mastering date", value: props.info.meta.mastering_date });
      }
      if (props.info.meta?.drc_use !== null && props.info.meta?.drc_use !== undefined) {
        f.push({
          label: "GamePad required",
          value: formatBool(props.info.meta.drc_use),
        });
      }
      if (props.info.meta?.app_size && props.info.meta.app_size > 0) {
        f.push({ label: "App size", value: formatBytes(props.info.meta.app_size) });
      }
      if (props.info.meta) {
        const accessories: string[] = [];
        if (props.info.meta.ext_dev_nunchaku) accessories.push("Nunchuk");
        if (props.info.meta.ext_dev_classic) accessories.push("Classic Controller");
        if (props.info.meta.ext_dev_urcc) accessories.push("URCC");
        if (props.info.meta.ext_dev_board) accessories.push("Balance Board");
        if (props.info.meta.ext_dev_usb_keyboard) accessories.push("USB Keyboard");
        if (accessories.length) {
          f.push({ label: "Accessories", value: accessories.join(", ") });
        }
      }
      break;
    }
    case "nx": {
      f.push({ label: "Container", value: props.info.container_kind.toUpperCase() });
      f.push({
        label: "Compressed",
        value: formatBool(props.info.is_compressed, props.info.is_compressed ? "zstd" : undefined),
      });
      f.push({ label: "Distribution", value: nxLabel(props.info.distribution) });
      f.push({ label: "Structure", value: nxLabel(props.info.structure) });
      f.push({ label: "NCA files", value: String(props.info.nca_names.length) });
      if (props.info.full) {
        f.push({
          label: "Title ID",
          value: props.info.full.application_title_id
            .toString(16)
            .padStart(16, "0")
            .toUpperCase(),
        });
        f.push({ label: "Title kind", value: nxTitleKindDisplayName(props.info.full.title_kind) });
        f.push({ label: "Title version", value: String(props.info.full.title_version) });
        if (props.info.full.base_application_id !== null) {
          f.push({
            label: "Base game",
            value: props.info.full.base_application_id
              .toString(16)
              .padStart(16, "0")
              .toUpperCase(),
          });
        }
        const ctrl = props.info.full.control;
        if (ctrl) {
          if (ctrl.display_version) {
            f.push({ label: "Display version", value: ctrl.display_version });
          }
          if (ctrl.startup_user_account_name) {
            f.push({ label: "Startup account", value: ctrl.startup_user_account_name });
          }
          if (ctrl.video_capture_name) {
            f.push({ label: "Video capture", value: ctrl.video_capture_name });
          }
          if (ctrl.screen_orientation_name) {
            f.push({ label: "Screen orientation", value: ctrl.screen_orientation_name });
          }
          if (ctrl.addon_install_policy_name) {
            f.push({ label: "Add-on install policy", value: ctrl.addon_install_policy_name });
          }
          if (ctrl.attributes.length) {
            f.push({ label: "Attributes", value: ctrl.attributes.join(", ") });
          }
          if (ctrl.parental_control_flags.length) {
            f.push({ label: "Parental control", value: ctrl.parental_control_flags.join(", ") });
          }
          if (ctrl.supported_languages.length) {
            f.push({ label: "Languages", value: ctrl.supported_languages.join(", ") });
          }
          if (ctrl.age_ratings.length) {
            f.push({
              label: "Age ratings",
              value: ctrl.age_ratings.map((r) => `${r.organization} ${r.age}+`).join(", "),
            });
          }
        }
      } else {
        f.push({ label: "Keys", value: "prod.keys not loaded" });
      }
      break;
    }
    case "chd": {
      f.push({ label: "Version", value: `CHD v${props.info.version}` });
      f.push({ label: "Hunks", value: String(props.info.hunk_count) });
      f.push({ label: "Hunk size", value: formatBytes(props.info.hunk_bytes) });
      f.push({
        label: "Compression ratio",
        value: `${props.info.compression_ratio.toFixed(2)}%`,
      });
      f.push({ label: "Raw SHA-1", value: props.info.raw_sha1 });
      f.push({ label: "SHA-1", value: props.info.sha1 });
      if (props.info.tracks.length > 0) {
        f.push({ label: "Tracks", value: String(props.info.tracks.length) });
      }
      break;
    }
    case "cso": {
      f.push({ label: "Format", value: `${props.info.format} v${props.info.version}` });
      f.push({ label: "Block size", value: formatBytes(props.info.block_size) });
      f.push({ label: "Index shift", value: String(props.info.index_shift) });
      f.push({
        label: "Blocks",
        value: `${props.info.block_count} (${props.info.raw_block_count} stored raw)`,
      });
      f.push({
        label: "Compression ratio",
        value: `${props.info.compression_ratio.toFixed(2)}%`,
      });
      f.push({ label: "Uncompressed size", value: formatBytes(props.info.uncompressed_size) });
      f.push({ label: "Physical size", value: formatBytes(props.info.physical_bytes) });
      break;
    }
  }
  return f;
});

const localizedTitles = computed<LocalizedEntry[]>(() => {
  switch (props.info.kind) {
    case "ctr": {
      const titles = props.info.smdh?.titles ?? [];
      return titles.map((t) => ({
        language: languageLabel(t.language),
        name: t.long_description || t.short_description,
        publisher: t.publisher,
      }));
    }
    case "rvl": {
      const entries = props.info.imet_names?.entries ?? [];
      return entries.map(([lang, name]) => ({
        language: languageLabel(lang),
        name,
        publisher: "",
      }));
    }
    case "wup": {
      const longs = props.info.meta?.long_names?.entries ?? [];
      const pubs = props.info.meta?.publishers?.entries ?? [];
      const pubByLang = new Map(pubs.map(([lang, name]) => [lang, name]));
      return longs.map(([lang, name]) => ({
        language: languageLabel(lang),
        name,
        publisher: pubByLang.get(lang) ?? "",
      }));
    }
    case "nx": {
      const titles = props.info.full?.control?.titles ?? [];
      return titles.map((t) => ({
        language: languageLabel(t.language),
        name: t.name,
        publisher: t.publisher,
      }));
    }
    case "dol": {
      const titles = props.info.banner?.titles ?? [];
      return titles.map((t) => ({
        language: languageLabel(t.language),
        name: t.long_game_name || t.short_game_name,
        publisher: t.long_maker || t.short_maker,
      }));
    }
    default:
      return [];
  }
});

const copied = ref(false);
let copyResetTimer: ReturnType<typeof setTimeout> | null = null;

function flagCopied() {
  copied.value = true;
  if (copyResetTimer !== null) {
    clearTimeout(copyResetTimer);
  }
  copyResetTimer = setTimeout(() => {
    copied.value = false;
    copyResetTimer = null;
  }, 1500);
}

function copyTitleId() {
  let value = "";
  switch (props.info.kind) {
    case "ctr":
      value = props.info.title_id;
      break;
    case "dol":
      value = props.info.game_id;
      break;
    case "wup":
      value = props.info.title_id_hex;
      break;
    case "rvl":
      if (props.info.tmd) {
        value = props.info.tmd.title_id.toString(16).padStart(16, "0").toUpperCase();
      } else {
        value = props.info.game_id;
      }
      break;
    case "nx":
      if (props.info.full) {
        value = props.info.full.application_title_id
          .toString(16)
          .padStart(16, "0")
          .toUpperCase();
      }
      break;
    default:
      return;
  }
  if (value && typeof navigator !== "undefined" && navigator.clipboard) {
    navigator.clipboard.writeText(value).then(flagCopied, () => {});
  }
}

async function saveIcon() {
  const dest = await save({
    defaultPath: "icon.png",
    filters: [{ name: "PNG", extensions: ["png"] }],
  });
  if (!dest) return;
  await invoke("cmd_save_icon", { infoJson: JSON.stringify(props.info), dest });
}
</script>

<template>
  <div class="rom-info-card">
    <div class="rom-info-card__header">
      <img
        v-if="iconUrl"
        :src="iconUrl"
        class="rom-info-card__icon"
        alt="ROM icon"
      />
      <div class="rom-info-card__title-block">
        <div class="rom-info-card__title">{{ title }}</div>
        <div v-if="subtitle" class="rom-info-card__subtitle">{{ subtitle }}</div>
      </div>
    </div>

    <dl class="rom-info-card__fields">
      <template v-for="field in fields" :key="field.label">
        <dt>{{ field.label }}</dt>
        <dd>{{ field.value }}</dd>
      </template>
    </dl>

    <section
      v-if="info.kind === 'wup' && info.bundled_titles && info.bundled_titles.length > 1"
      class="rom-info-card__locales"
    >
      <h4 class="rom-info-card__locales-heading">Bundled titles</h4>
      <ul class="rom-info-card__locales-list">
        <li
          v-for="bt in info.bundled_titles"
          :key="bt.title_id_hex"
          class="rom-info-card__locale"
        >
          <span class="rom-info-card__locale-lang">{{ bt.title_type }}</span>
          <span class="rom-info-card__locale-name">{{ bt.title_id_hex }}</span>
          <span class="rom-info-card__locale-pub">v{{ bt.title_version }}</span>
        </li>
      </ul>
    </section>

    <section v-if="localizedTitles.length > 1" class="rom-info-card__locales">
      <h4 class="rom-info-card__locales-heading">Localized titles</h4>
      <ul class="rom-info-card__locales-list">
        <li
          v-for="entry in localizedTitles"
          :key="entry.language + entry.name"
          class="rom-info-card__locale"
        >
          <span class="rom-info-card__locale-lang">{{ entry.language }}</span>
          <span class="rom-info-card__locale-name">{{ entry.name }}</span>
          <span v-if="entry.publisher" class="rom-info-card__locale-pub">by {{ entry.publisher }}</span>
        </li>
      </ul>
    </section>

    <div v-if="(info.kind !== 'chd' && info.kind !== 'cso') || iconUrl" class="rom-info-card__footer">
      <button
        v-if="info.kind !== 'chd' && info.kind !== 'cso'"
        type="button"
        class="rom-info-card__btn"
        @click="copyTitleId"
      >
        {{ copied ? "Copied" : "Copy title ID" }}
      </button>
      <button v-if="iconUrl" type="button" class="rom-info-card__btn" @click="saveIcon">
        Save icon
      </button>
    </div>
  </div>
</template>

<style scoped>
.rom-info-card {
  --color-accent: #38bdf8;
  --color-accent-strong: #0ea5e9;
  --color-border: #3f3f46;
  --color-card-bg: rgba(39, 39, 42, 0.3);
  --color-text-muted: #a1a1aa;

  display: flex;
  flex-direction: column;
  gap: 1rem;
  padding: 1rem;
  border: 1px solid var(--color-border);
  border-radius: 0.5rem;
  background: var(--color-card-bg);
}

.rom-info-card__header {
  display: flex;
  gap: 1rem;
  align-items: center;
}

.rom-info-card__icon {
  max-width: 192px;
  max-height: 64px;
  width: auto;
  height: auto;
  border-radius: 0.5rem;
  object-fit: contain;
  image-rendering: pixelated;
  background: rgba(255, 255, 255, 0.02);
}

.rom-info-card__title {
  font-size: 1.25rem;
  font-weight: 600;
}

.rom-info-card__subtitle {
  color: var(--color-text-muted);
  font-size: 0.875rem;
}

.rom-info-card__fields {
  display: grid;
  grid-template-columns: max-content 1fr;
  column-gap: 1rem;
  row-gap: 0.25rem;
  font-family: var(--font-mono, monospace);
  font-size: 0.875rem;
}

.rom-info-card__fields dt {
  color: var(--color-text-muted);
}

.rom-info-card__fields dd {
  margin: 0;
  word-break: break-all;
}

.rom-info-card__footer {
  display: flex;
  gap: 0.5rem;
}

.rom-info-card__btn {
  padding: 0.4rem 0.9rem;
  border-radius: 0.375rem;
  background: var(--color-accent);
  color: white;
  border: none;
  cursor: pointer;
  font-size: 0.875rem;
}

.rom-info-card__btn:hover {
  background: var(--color-accent-strong);
}

.rom-info-card__locales {
  border-top: 1px solid var(--color-border);
  padding-top: 0.75rem;
}

.rom-info-card__locales-heading {
  margin: 0 0 0.5rem 0;
  font-size: 0.85rem;
  font-weight: 600;
  color: var(--color-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

.rom-info-card__locales-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: grid;
  grid-template-columns: auto 1fr auto;
  column-gap: 0.75rem;
  row-gap: 0.25rem;
  font-size: 0.875rem;
}

.rom-info-card__locale {
  display: grid;
  grid-template-columns: subgrid;
  grid-column: 1 / -1;
  align-items: baseline;
}

.rom-info-card__locale-lang {
  color: var(--color-text-muted);
  font-variant: small-caps;
}

.rom-info-card__locale-name {
  word-break: break-word;
}

.rom-info-card__locale-pub {
  color: var(--color-text-muted);
  font-size: 0.8rem;
}
</style>

// TypeScript mirror of crate::info::InfoResult. Kept hand-written rather
// than generated; the surface is small enough to maintain in lockstep
// with the Rust side. Field names match serde's `rename_all = "snake_case"`.

export interface Image {
  png_bytes: number[];
  width: number;
  height: number;
}

export type LanguageCode =
  | "japanese"
  | "english"
  | "american_english"
  | "british_english"
  | "french"
  | "canadian_french"
  | "german"
  | "italian"
  | "spanish"
  | "latin_american_spanish"
  | "dutch"
  | "portuguese"
  | "brazilian_portuguese"
  | "russian"
  | "korean"
  | "simplified_chinese"
  | "traditional_chinese"
  | "chinese"
  | "taiwanese_chinese";

export interface MultilingualString {
  entries: Array<[LanguageCode, string]>;
}

export interface ChdInfo {
  version: number;
  compressors: string[];
  hunk_bytes: number;
  unit_bytes: number;
  hunk_count: number;
  logical_bytes: number;
  physical_bytes: number;
  compression_ratio: number;
  raw_sha1: string;
  sha1: string;
  parent_sha1: string | null;
  tracks: Array<{
    number: number;
    track_type: string;
    frames: number;
    pregap: number;
    subtype: string | null;
    pgtype: string | null;
    pgsub: string | null;
    postgap: number | null;
  }>;
  metadata_tags: Array<{ tag: string; length: number }>;
  version_string: string | null;
  dvd: { total_sectors: number; layer_class: string } | null;
}

export interface CtrSmdhTitle {
  language: string;
  short_description: string;
  long_description: string;
  publisher: string;
}

export interface CtrInfo {
  format: "cia" | "ncsd" | "ncch" | "unknown";
  physical_bytes: number;
  title_id: string;
  program_id: string;
  product_code: string;
  maker_code: string;
  maker_name: string | null;
  cartridge_size: number | null;
  ncch_encrypted: boolean;
  smdh: {
    titles: CtrSmdhTitle[];
    region_lock: number;
    region_names: string[];
    flags: number;
    eula_version_major: number;
    eula_version_minor: number;
    age_ratings: Array<{
      region: string;
      age: number;
      pending: boolean;
      banned: boolean;
    }>;
  } | null;
  icon: Image | null;
  small_icon: Image | null;
}

export interface DolInfo {
  physical_bytes: number;
  game_id: string;
  maker_code: string;
  maker_name: string | null;
  disc_number: number;
  disc_version: number;
  audio_streaming: boolean;
  game_name: string;
  region: string;
  apploader_date: string | null;
  banner: {
    format: string;
    titles: Array<{
      language: string;
      short_game_name: string;
      short_maker: string;
      long_game_name: string;
      long_maker: string;
      description: string;
    }>;
  } | null;
  banner_image: Image | null;
}

export interface RvlInfo {
  physical_bytes: number;
  game_id: string;
  maker_code: string;
  maker_name: string | null;
  disc_number: number;
  disc_version: number;
  game_name: string;
  region: string;
  partitions: Array<{
    offset: number;
    partition_type: number;
    group: number;
    kind: string;
  }>;
  tmd: {
    title_id: number;
    title_version: number;
    system_version: number;
    ios_slot: number | null;
    region_name: string;
    content_count: number;
    access_rights: number;
  } | null;
  imet_names: MultilingualString | null;
  image: Image | null;
}

export interface BundledTitle {
  title_id: number;
  title_id_hex: string;
  title_type: string;
  title_version: number;
}

export interface WupInfo {
  title_id: number;
  title_id_hex: string;
  title_type: string;
  title_version: number;
  group_id: number;
  access_rights: number;
  content_count: number;
  total_content_size: number;
  os_version: number | null;
  sdk_version: number | null;
  source_kind: string;
  bundled_titles: BundledTitle[];
  update_version: number | null;
  image: Image | null;
  meta: {
    long_names: MultilingualString;
    short_names: MultilingualString;
    publishers: MultilingualString;
    product_code: string | null;
    company_code: string | null;
    company_name: string | null;
    region: number | null;
    region_names: string[];
    title_id: number | null;
    os_version: number | null;
    app_size: number | null;
    group_id: number | null;
    boss_id: number | null;
    mastering_date: string | null;
    content_platform: string | null;
    logo_type: number | null;
    app_launch_type: number | null;
    invisible_flag: boolean | null;
    no_managed_flag: boolean | null;
    eula_version: number | null;
    drc_use: boolean | null;
    e_manual: boolean | null;
    e_manual_version: number | null;
    ext_dev_nunchaku: boolean | null;
    ext_dev_classic: boolean | null;
    ext_dev_urcc: boolean | null;
    ext_dev_board: boolean | null;
    ext_dev_usb_keyboard: boolean | null;
    ext_dev_etc: boolean | null;
    ext_dev_etc_name: string | null;
    save_size: number | null;
    common_save_size: number | null;
    account_save_size: number | null;
    boss_size: number | null;
    common_boss_size: number | null;
    account_boss_size: number | null;
    network_use: boolean | null;
    online_account_use: boolean | null;
    age_ratings: Record<string, number>;
  } | null;
}

export interface NxContainerFile {
  partition: string | null;
  name: string;
  abs_offset: number;
  size: number;
}

export interface NxInfo {
  container_kind: "nsp" | "nsz" | "xci" | "xcz";
  is_compressed: boolean;
  distribution: "digital" | "cartridge";
  structure: "unknown" | "scene" | "converted" | "cdn" | "homebrew";
  physical_bytes: number;
  files: NxContainerFile[];
  nca_names: string[];
  cnmt_nca_names: string[];
  tickets: Array<{
    file_name: string;
    rights_id: string;
    master_key_revision: number;
  }>;
  xci_partitions:
    | Array<{ name: string; file_count: number; total_size: number }>
    | null;
  full: {
    application_title_id: number;
    title_version: number;
    title_kind: string;
    storage_id: number;
    attributes: number;
    required_system_version: number;
    required_application_version: number | null;
    base_application_id: number | null;
    content_count: number;
    total_content_size: number;
    contents: Array<{
      content_id: string;
      content_type: string;
      size: number;
    }>;
    related_titles: Array<{
      title_id: number;
      kind: string;
      version: number;
    }>;
    control: {
      titles: Array<{ language: string; name: string; publisher: string }>;
      display_version: string;
      startup_user_account: number;
      startup_user_account_name: string;
      screenshot: number;
      video_capture: number;
      video_capture_name: string;
      attribute_flag: number;
      attributes: string[];
      supported_language_bitmask: number;
      supported_languages: string[];
      parental_control_flag: number;
      parental_control_flags: string[];
      user_account_save: number;
      user_account_save_journal: number;
      device_save: number;
      device_save_journal: number;
      bcat_save: number;
      rating_age: number[];
      age_ratings: Array<{ organization: string; age: number }>;
      addon_install_policy: number;
      addon_install_policy_name: string;
      screen_orientation: number;
      screen_orientation_name: string;
      icon: Image | null;
      icon_language: string | null;
    } | null;
  } | null;
}

export type InfoResult =
  | ({ kind: "chd" } & ChdInfo)
  | ({ kind: "ctr" } & CtrInfo)
  | ({ kind: "dol" } & DolInfo)
  | ({ kind: "rvl" } & RvlInfo)
  | ({ kind: "wup" } & WupInfo)
  | ({ kind: "nx" } & NxInfo);

export function nxTitleKindDisplayName(kind: string): string {
  switch (kind) {
    case "application":
      return "Game";
    case "patch":
      return "Update";
    case "add_on_content":
      return "DLC";
    case "delta":
      return "Delta";
    case "system_program":
      return "System Program";
    case "system_data":
      return "System Data";
    case "system_update":
      return "System Update";
    case "unknown":
      return "Unknown";
    default:
      return kind;
  }
}

export function pickIconImage(info: InfoResult): Image | null {
  switch (info.kind) {
    case "ctr":
      return info.icon;
    case "dol":
      return info.banner_image;
    case "rvl":
      return info.image;
    case "wup":
      return info.image;
    case "nx":
      return info.full?.control?.icon ?? null;
    default:
      return null;
  }
}

export function imageToDataUrl(img: Image): string {
  const bytes = new Uint8Array(img.png_bytes);
  let binary = "";
  bytes.forEach((byte) => {
    binary += String.fromCharCode(byte);
  });
  const base64 = typeof btoa !== "undefined" ? btoa(binary) : "";
  return `data:image/png;base64,${base64}`;
}

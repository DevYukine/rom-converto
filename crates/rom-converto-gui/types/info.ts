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

export interface LdClvTime {
  hours: number;
  minutes: number;
}

export interface ChdLdInfo {
  fps: string;
  width: number;
  height: number;
  interlaced: boolean;
  channels: number;
  sample_rate: number;
  frame_count: number;
  vbi: {
    disc_type: "cav" | "clv" | "unknown";
    white_flag_count: number;
    cav_picture_min: number | null;
    cav_picture_max: number | null;
    clv_start_time: LdClvTime | null;
    clv_end_time: LdClvTime | null;
    chapter_min: number | null;
    chapter_max: number | null;
    lead_in: boolean;
    lead_out: boolean;
  } | null;
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
  ld: ChdLdInfo | null;
  content: DiscContent | null;
}

export interface CsoInfo {
  format: string;
  version: number;
  block_size: number;
  index_shift: number;
  uncompressed_size: number;
  physical_bytes: number;
  compression_ratio: number;
  block_count: number;
  raw_block_count: number;
  content: DiscContent | null;
}

export interface CtrSmdhTitle {
  language: string;
  short_description: string;
  long_description: string;
  publisher: string;
}

export interface CtrPartitionEntry {
  index: number;
  name: string;
  offset: number;
  size: number;
}

export interface CtrContentEntry {
  index: number;
  content_id: string;
  size: number;
  encrypted: boolean;
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
  compressed: boolean;
  ncsd_partitions: CtrPartitionEntry[];
  cia_contents: CtrContentEntry[];
}

export interface DolFstEntry {
  name: string;
  size: number;
  is_dir: boolean;
}

export interface DolInfo {
  physical_bytes: number;
  container: string;
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
  fst_root: DolFstEntry[];
  fst_file_count: number;
  fst_dir_count: number;
}

export interface RvlInfo {
  physical_bytes: number;
  container: string;
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
    title_id_hex: string;
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

export interface WupDiscPartition {
  name: string;
  kind: string;
  start_sector: number;
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
  disc_partitions: WupDiscPartition[];
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
    application_title_id_hex: string;
    title_version: number;
    title_kind: string;
    storage_id: number;
    attributes: number;
    required_system_version: number;
    required_application_version: number | null;
    base_application_id: number | null;
    base_application_id_hex: string | null;
    content_count: number;
    total_content_size: number;
    contents: Array<{
      content_id: string;
      content_type: string;
      size: number;
    }>;
    related_titles: Array<{
      title_id: number;
      title_id_hex: string;
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

export interface XbeInfo {
  title_id: number;
  title_id_hex: string;
  title_id_code: string;
  title_name: string;
  alternate_title_ids: number[];
  allowed_media: number;
  allowed_media_names: string[];
  region: number;
  region_names: string[];
  ratings: number;
  disc_number: number;
  version: number;
  cert_timestamp: number;
  icon: Image | null;
}

export interface XexInfo {
  title_id: number;
  title_id_hex: string;
  media_id: number;
  version: string;
  version_raw: number;
  base_version: string;
  base_version_raw: number;
  disc_number: number;
  disc_count: number;
  platform: number;
  original_pe_name: string | null;
  region: number;
  region_names: string[];
  allowed_media: number;
  title_name: string | null;
  icon: Image | null;
}

export interface XisoRootEntry {
  name: string;
  size: number;
  is_dir: boolean;
}

export interface XboxInfo {
  partition_kind: "trimmed" | "xgd1" | "xgd2" | "xgd3" | { x360_extra: number };
  base: number;
  root_sector: number;
  root_size: number;
  file_count: number;
  dir_count: number;
  total_file_bytes: number;
  image_size: number;
  xbe: XbeInfo | null;
  xex: XexInfo | null;
  root_entries: XisoRootEntry[];
}

export interface ZarRootEntry {
  name: string;
  size: number;
  is_file: boolean;
}

export interface XenonInfo {
  file_count: number;
  dir_count: number;
  logical_size: number;
  compressed_size: number;
  block_count: number;
  has_default_xex: boolean;
  xex: XexInfo | null;
  root_entries: ZarRootEntry[];
}

export interface Ps3RootEntry {
  name: string;
  size: number;
  is_dir: boolean;
}

export interface Ps3Info {
  title: string | null;
  title_id: string | null;
  region: string | null;
  version: string | null;
  app_ver: string | null;
  resolution: string | null;
  sound_format: string | null;
  firmware: string | null;
  parental_level: number | null;
  region_count: number;
  total_sectors: number;
  encrypted_sectors: number;
  encrypted: boolean | null;
  size_bytes: number;
  icon: Image | null;
  root_files: Ps3RootEntry[];
}

export interface PsxInfo {
  disc_kind: string;
  boot_executable: string | null;
  title_id: string | null;
  volume_id: string | null;
  version: string | null;
  total_sectors: number;
  size_bytes: number;
}

export interface PspInfo {
  title: string | null;
  title_id: string | null;
  version: string | null;
  firmware: string | null;
  category: string | null;
  total_sectors: number;
  size_bytes: number;
  icon: Image | null;
  background: Image | null;
}

export interface LdVbiSummary {
  fields_scanned: number;
  white_flag_count: number;
  lead_in: boolean;
  lead_out: boolean;
  disc_type: "cav" | "clv" | "unknown";
  cav_picture_min: number | null;
  cav_picture_max: number | null;
  clv_start: LdClvTime | null;
  clv_end: LdClvTime | null;
  chapter_min: number | null;
  chapter_max: number | null;
  fields_without_code: number;
}

export interface LdAviInfo {
  video_fourcc: string;
  video_width: number;
  video_height: number;
  fps: number;
  frame_count: number;
  duration_seconds: number;
  audio_channels: number;
  audio_rate: number;
  audio_bits: number;
  audio_sample_count: number;
  file_size_bytes: number;
  interlaced: boolean;
  field_height: number;
  fields: number;
  max_samples_per_field: number;
  bytes_per_frame: number;
  fps_times_1million: number;
  av_metadata: string;
  vbi: LdVbiSummary | null;
}

export type DiscContent =
  | ({ kind: "psx" } & PsxInfo)
  | ({ kind: "psp" } & PspInfo);

export interface NdsArmInfo {
  rom_offset: number;
  entry_address: number;
  load_address: number;
  size: number;
}

export type NdsSecureAreaState = "not_present" | "encrypted" | "decrypted";

export interface NdsBannerInfo {
  banner_version: number;
  titles: MultilingualString;
  banner_crc16: number;
  banner_crc16_computed: number;
  banner_crc16_valid: boolean;
  icon: Image;
}

export interface NdsInfo {
  physical_bytes: number;
  game_title: string;
  game_code: string;
  maker_code: string;
  unit_code: number;
  unit_code_name: string;
  region: number;
  rom_version: number;
  device_capacity: number;
  capacity_bytes: number;
  ntr_rom_size: number;
  arm9: NdsArmInfo;
  arm7: NdsArmInfo;
  fnt_offset: number;
  fnt_size: number;
  fat_offset: number;
  fat_size: number;
  header_crc16: number;
  header_crc16_computed: number;
  header_crc16_valid: boolean;
  secure_area: NdsSecureAreaState;
  banner: NdsBannerInfo | null;
}

export type InfoResult =
  | ({ kind: "chd" } & ChdInfo)
  | ({ kind: "cso" } & CsoInfo)
  | ({ kind: "ctr" } & CtrInfo)
  | ({ kind: "dol" } & DolInfo)
  | ({ kind: "rvl" } & RvlInfo)
  | ({ kind: "wup" } & WupInfo)
  | ({ kind: "nx" } & NxInfo)
  | ({ kind: "xbox" } & XboxInfo)
  | ({ kind: "xenon" } & XenonInfo)
  | ({ kind: "ps3" } & Ps3Info)
  | ({ kind: "psx" } & PsxInfo)
  | ({ kind: "psp" } & PspInfo)
  | ({ kind: "laser_disc" } & LdAviInfo)
  | ({ kind: "nds" } & NdsInfo);

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
    case "xbox":
      return info.xbe?.icon ?? info.xex?.icon ?? null;
    case "xenon":
      return info.xex?.icon ?? null;
    case "chd":
    case "cso":
      return info.content?.kind === "psp" ? info.content.icon : null;
    case "ps3":
      return info.icon;
    case "psp":
      return info.icon;
    case "nds":
      return info.banner?.icon ?? null;
    default:
      return null;
  }
}

export function pickBackgroundImage(info: InfoResult): Image | null {
  switch (info.kind) {
    case "chd":
    case "cso":
      return info.content?.kind === "psp" ? info.content.background : null;
    case "psp":
      return info.background;
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

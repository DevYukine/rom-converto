import type { Image, InfoResult } from "~/types";

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
    case "ntr":
      return info.banner?.icon ?? null;
    case "pbp":
      return info.icon;
    case "vpk":
      return info.icon;
    case "pkg":
      return info.icon;
    case "ps4_pkg":
    case "ps5_pkg":
      return info.icon;
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
    case "vpk":
    case "pkg":
      return info.background;
    case "ps4_pkg":
    case "ps5_pkg":
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

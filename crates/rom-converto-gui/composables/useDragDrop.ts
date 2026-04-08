import { ref } from "vue";
import { getCurrentWebview } from "@tauri-apps/api/webview";

type DropHandler = (paths: string[]) => void;

interface DropZone {
  el: HTMLElement;
  handler: DropHandler;
  priority: number; // lower = receives fallback drops (first input field on page)
}

const zones = new Map<string, DropZone>();
let activeZoneId: string | null = null;
let initialized = false;

// Reactive state for the full-screen overlay
export const isDraggingOver = ref(false);

function findZoneAtPoint(x: number, y: number): string | null {
  for (const [id, zone] of zones) {
    const rect = zone.el.getBoundingClientRect();
    if (x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom) {
      return id;
    }
  }
  return null;
}

function getPrimaryZone(): string | null {
  let best: { id: string; priority: number } | null = null;
  for (const [id, zone] of zones) {
    if (!best || zone.priority < best.priority) {
      best = { id, priority: zone.priority };
    }
  }
  return best?.id ?? null;
}

function initGlobalListener() {
  if (initialized) return;
  initialized = true;

  getCurrentWebview().onDragDropEvent((event) => {
    const payload = event.payload;

    if (payload.type === "over") {
      isDraggingOver.value = true;
      const pos = payload.position;
      const newActive = findZoneAtPoint(pos.x, pos.y);

      if (activeZoneId && activeZoneId !== newActive) {
        zones.get(activeZoneId)?.el.classList.remove("drop-hover");
      }

      activeZoneId = newActive;

      if (activeZoneId) {
        zones.get(activeZoneId)?.el.classList.add("drop-hover");
      }
    }

    if (payload.type === "drop") {
      isDraggingOver.value = false;

      const targetId = activeZoneId ?? getPrimaryZone();

      if (targetId) {
        const zone = zones.get(targetId);
        zone?.el.classList.remove("drop-hover");
        zone?.handler(payload.paths);
      }
      activeZoneId = null;
    }

    if (payload.type === "leave") {
      isDraggingOver.value = false;
      if (activeZoneId) {
        zones.get(activeZoneId)?.el.classList.remove("drop-hover");
      }
      activeZoneId = null;
    }
  });
}

let nextId = 0;

export function registerDropZone(
  el: HTMLElement,
  handler: DropHandler,
  priority = 100,
): string {
  initGlobalListener();
  const id = `dropzone-${nextId++}`;
  zones.set(id, { el, handler, priority });
  return id;
}

export function unregisterDropZone(id: string) {
  zones.delete(id);
}

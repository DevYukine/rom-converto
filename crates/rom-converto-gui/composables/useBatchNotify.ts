import { getCurrentWindow, ProgressBarStatus } from "@tauri-apps/api/window";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";

const KIB = 1024;

// Mirrors rom_converto_lib::util::tally::format_bytes so the notification
// summary matches the CLI's byte formatting.
export function formatBytes(n: number): string {
  if (n < KIB) return `${Math.trunc(n)} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let value = n / KIB;
  let unit = 0;
  while (value >= KIB && unit < units.length - 1) {
    value /= KIB;
    unit++;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

// 150ms sine beep, 8kHz/8-bit mono WAV, inlined so it works under the app's CSP.
const BEEP_DATA_URI =
  "data:audio/wav;base64,UklGRtQEAABXQVZFZm10IBAAAAABAAEAQB8AAEAfAAABAAgAZGF0YbAEAACA0f3vrlgUAil4yvrytWAaAiRww/f0vGgfAx9ou/P2wnAlBBthtO/3yHgrBhharev4zX8yCBVTpeb404c4ChJMnuH4148/DRBGltz33JZGEQ5Ajtb2351NFQ06h9D046RUGQw1gMry5qpcHQwweMTw6LFjIg0scb3t6rdqJw0oarbp7LxxLQ4kY7Dm7cJ4MxAhXani7ceAOBIeV6Ld7cuGPxUcUZvY7dCNRRcaS5TT7NOUSxsYRY3O69eaUh4XQIbJ6tqgWCIXPIDD6NymXyYXN3m95d6sZSsXM3K34+CybDAYMGyx3+K3czUZLWar3OK7eTobKmCl2OPAfz8dJ1qf1OPEhkUfJVWY0OPIjEoiJFCSy+LLklAlI0uMx+HOmFYoIkeGwt/RnVwrIUJ/vN3To2IvIT96t9vVqGgzIjt0stjWrG44IzhurNbXsXQ8JDVpp9LYtXpBJTNjoc/YuYBGJzFem8vYvYVLKS9alsfYwItQLC5VkMPXw5BVLi1Rir/WxpVbMSxNhbrVyJpgNSxJgLbTyp9lOCxGerHRy6NrPCxDdazOzKdwQC1AcKfMzat1RC4+a6LJzq96SDA8Z53GzrJ/TTE6YpjCzrWEUTM5XpO/zbiJVjY3Wo67zbuOWzg3V4m3y72SXzs2U4Szyr+XZD42UICvyMCbaUE2TXurxsKfbUU3S3amxMOickg4SHKiwsOmd0w5Rm6dv8Spe1A6RWqZvMSsf1Q8Q2aVucOuhFg+QmOQtsOxiFxAQV+Ms8KzjGBCQVyIr8G0kGRFQVmErL+2k2hHQVd/qL63l2xKQVR8pLy4mnBNQlJ4obq5nXRQQlF0nbi5oHhUQ09xmba5onxXRU5tlbO5pX9aRk1qkbC5p4NeSExnjq24qYdhSktkiqu3qoplTEtihqi2rI1oTktgg6S1rZBsUUtef6GzrpNvU0xcfJ6yrpZzVkxaeZuwr5h2WU1Zdpiur5p5W05Yc5Ssr5x8Xk9XcZGqr55/YVFWbo6nrqCCZFJVbIulrqGFZ1RVaoiiraKIalZVaIWgrKOKbVhVZoKdqqSNcFpWZICbqaSPc1xWY32YqKWRdV5XYnqVpqWTeGFYYXiTpKWVe2NZYHaQoqWWfWVaYHSNoKSXf2hbX3KLnqSYgmpdX3CInKOZhG1eX2+GmqKahm9gX22EmKGbiHFiX2yClqCbinRjYGuAlJ+bi3ZlYWp+kp2bjHhnYWp8j5ybjnppYml6jZqbj3xrY2l5i5makH5tZGl3iZeakH9vZml2iJWZkYFxZ2l1hpSYkoNyaGl0hJKXkoR0amlzgpCWkoV2a2pygY+VkoZ4bWpygI2Ukod5bmtyfouTkoh7cGxxfYqSkYl8cW1xfIiQkYl9c25xe4ePkIl+dG9yeoaOj4p/dXByeoSMj4qAd3FyeYOLjoqBeHJzeYKKjYqCeXRzeYGJjImDenV0eYCHi4mDe3Z1eYCGiomDfHd2eX+FiYiDfXh3eX6EiIeDfnl4eX6Dh4eDf3p5en6ChYaDf3t5en6ChIWDgHx6e36Bg4SDgH17fH6Ag4OCgH58fX6AgoKBgH59fX6AgYGBgH9+fn+AgICAgH9/f38=";

async function ensurePermission(): Promise<boolean> {
  try {
    if (await isPermissionGranted()) return true;
    return (await requestPermission()) === "granted";
  } catch {
    return false;
  }
}

function playBeep() {
  try {
    new Audio(BEEP_DATA_URI).play().catch(() => {});
  } catch {
    // Audio playback unavailable; skip the sound.
  }
}

export function useBatchNotify() {
  async function setTaskbarProgress(fraction: number) {
    try {
      await getCurrentWindow().setProgressBar({
        status: ProgressBarStatus.Normal,
        progress: Math.max(0, Math.min(100, Math.round(fraction * 100))),
      });
    } catch {
      // No taskbar/dock progress support on this platform.
    }
  }

  async function taskbarError() {
    try {
      await getCurrentWindow().setProgressBar({ status: ProgressBarStatus.Error });
    } catch {
      // No taskbar/dock progress support on this platform.
    }
  }

  async function clearTaskbar() {
    try {
      await getCurrentWindow().setProgressBar({ status: ProgressBarStatus.None });
    } catch {
      // No taskbar/dock progress support on this platform.
    }
  }

  async function notifyBatchDone(title: string, body: string, sound: boolean) {
    if (sound) playBeep();
    try {
      if (!(await ensurePermission())) return;
      sendNotification({ title, body });
    } catch {
      // Notifications unsupported on this platform; the in-app summary still shows.
    }
  }

  return { formatBytes, setTaskbarProgress, taskbarError, clearTaskbar, notifyBatchDone };
}

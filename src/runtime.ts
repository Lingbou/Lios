import { invoke, isTauri as coreIsTauri } from "@tauri-apps/api/core";

export function isTauri(): boolean {
  if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
    return true;
  }
  return typeof coreIsTauri === "function" ? coreIsTauri() : false;
}

export { invoke as appInvoke };
export const hasTauriRuntime = isTauri;

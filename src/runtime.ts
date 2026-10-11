import { invoke } from "@tauri-apps/api/core";

export function hasTauriRuntime() {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export const appInvoke = invoke;
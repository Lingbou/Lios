import { type MouseEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { hasTauriRuntime } from "../runtime.ts";

export function useWindowControls(onError: (error: unknown) => void) {
  async function minimizeWindow() {
    if (!hasTauriRuntime()) return;
    try {
      await getCurrentWindow().minimize();
    } catch (error) {
      onError(error);
    }
  }

  async function toggleMaximizeWindow() {
    if (!hasTauriRuntime()) return;
    try {
      await getCurrentWindow().toggleMaximize();
    } catch (error) {
      onError(error);
    }
  }

  async function closeWindow() {
    if (!hasTauriRuntime()) return;
    try {
      await getCurrentWindow().close();
    } catch (error) {
      onError(error);
    }
  }

  async function startWindowDrag(event: MouseEvent<HTMLElement>) {
    if (event.button !== 0 || !hasTauriRuntime()) return;
    event.preventDefault();
    try {
      await getCurrentWindow().startDragging();
    } catch (error) {
      onError(error);
    }
  }

  return {
    minimizeWindow,
    toggleMaximizeWindow,
    closeWindow,
    startWindowDrag
  };
}
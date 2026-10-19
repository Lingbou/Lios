import { type MouseEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isTauri } from "../runtime.ts";

export function useWindowControls(onError: (error: unknown) => void) {
  async function minimizeWindow() {
    if (!isTauri()) return;
    try {
      await getCurrentWindow().minimize();
    } catch (error) {
      onError(error);
    }
  }

  async function toggleMaximizeWindow() {
    if (!isTauri()) return;
    try {
      await getCurrentWindow().toggleMaximize();
    } catch (error) {
      onError(error);
    }
  }

  async function closeWindow() {
    if (!isTauri()) return;
    try {
      await getCurrentWindow().close();
    } catch (error) {
      onError(error);
    }
  }

  async function startWindowDrag(event: MouseEvent<HTMLElement>) {
    if (event.button !== 0 || !isTauri()) return;
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
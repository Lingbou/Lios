import { expect, test, vi } from "vitest";
import { installBrowserContextMenuGuard } from "../../src/browserContextMenu.ts";

test("blocks the browser menu without swallowing application context-menu events", () => {
  const cleanup = installBrowserContextMenuGuard(document);
  const applicationListener = vi.fn();
  document.body.addEventListener("contextmenu", applicationListener);

  try {
    const event = new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      button: 2
    });

    expect(document.body.dispatchEvent(event)).toBe(false);
    expect(event.defaultPrevented).toBe(true);
    expect(applicationListener).toHaveBeenCalledOnce();
  } finally {
    document.body.removeEventListener("contextmenu", applicationListener);
    cleanup();
  }
});

test("restores the default behavior when the guard is removed", () => {
  const cleanup = installBrowserContextMenuGuard(document);
  cleanup();

  const event = new MouseEvent("contextmenu", {
    bubbles: true,
    cancelable: true,
    button: 2
  });

  expect(document.body.dispatchEvent(event)).toBe(true);
  expect(event.defaultPrevented).toBe(false);
});

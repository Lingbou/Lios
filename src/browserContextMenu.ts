export function installBrowserContextMenuGuard(target: Document): () => void {
  const preventBrowserContextMenu = (event: Event) => {
    event.preventDefault();
  };

  target.addEventListener("contextmenu", preventBrowserContextMenu, true);

  return () => {
    target.removeEventListener("contextmenu", preventBrowserContextMenu, true);
  };
}

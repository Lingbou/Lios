import "@testing-library/jest-dom/vitest";

Object.defineProperty(globalThis, "ResizeObserver", {
  configurable: true,
  value: class ResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
});

HTMLElement.prototype.getBoundingClientRect = function getBoundingClientRect() {
  return {
    x: 0,
    y: 0,
    top: 0,
    left: 0,
    right: 800,
    bottom: 240,
    width: 800,
    height: 240,
    toJSON: () => ({})
  };
};

const createMemoryStorage = (): Storage => {
  const store = new Map<string, string>();
  return {
    get length(): number {
      return store.size;
    },
    clear(): void {
      store.clear();
    },
    getItem(key: string): string | null {
      return store.get(key) ?? null;
    },
    key(index: number): string | null {
      return Array.from(store.keys())[index] ?? null;
    },
    removeItem(key: string): void {
      store.delete(key);
    },
    setItem(key: string, value: string): void {
      store.set(key, String(value));
    }
  };
};

// Node 22+ exposes an experimental `globalThis.localStorage` that stays undefined unless
// `--localstorage-file` is passed, and it can shadow the jsdom implementation. Read it defensively
// and install one shared in-memory Storage so both entry points behave identically.
const readStorage = (target: Window | typeof globalThis): Storage | undefined => {
  try {
    const storage = target.localStorage;
    return storage && typeof storage.clear === "function" ? storage : undefined;
  } catch {
    return undefined;
  }
};

const installStorage = (target: Window | typeof globalThis, storage: Storage): void => {
  try {
    Object.defineProperty(target, "localStorage", {
      value: storage,
      configurable: true,
      writable: true
    });
  } catch {
    // ignore
  }
};

const fallbackStorage = createMemoryStorage();
if (!readStorage(window)) {
  installStorage(window, fallbackStorage);
}
if (!readStorage(globalThis)) {
  installStorage(globalThis, readStorage(window) ?? fallbackStorage);
}

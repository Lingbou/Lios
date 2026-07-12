import { commandError } from "./commandError.ts";

export type LatestSerialRequest = {
  isCurrent: () => boolean;
};

export function createLatestSerialExecutor() {
  let latestRequest = 0;
  let tail: Promise<void> = Promise.resolve();

  return {
    run<T>(operation: (request: LatestSerialRequest) => Promise<T>): Promise<T | undefined> {
      const requestId = ++latestRequest;
      const request: LatestSerialRequest = {
        isCurrent: () => requestId === latestRequest
      };
      const result = tail.then(async () => {
        if (!request.isCurrent()) return undefined;
        return operation(request);
      });
      tail = result.then(
        () => undefined,
        () => undefined
      );
      return result;
    }
  };
}

export type CatalogLoadState<T> =
  | { status: "ready"; catalog: T }
  | { status: "missing" };

export async function loadCatalogState<T>(load: () => Promise<T>): Promise<CatalogLoadState<T>> {
  try {
    return { status: "ready", catalog: await load() };
  } catch (error) {
    if (commandError(error)?.code === "NotInitialized") {
      return { status: "missing" };
    }
    throw error;
  }
}

export async function initializeWithExistingCatalog<T>(
  initialize: () => Promise<T>,
  reload: () => Promise<T>
): Promise<T> {
  try {
    return await initialize();
  } catch (error) {
    if (commandError(error)?.code === "AlreadyInitialized") {
      return reload();
    }
    throw error;
  }
}

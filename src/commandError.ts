export type CommandErrorCode =
  | "NotInitialized"
  | "AlreadyInitialized"
  | "Authentication"
  | "Network"
  | "WrongKey"
  | "RemoteConflict"
  | "RateLimited"
  | "RemoteServer"
  | "CorruptedData"
  | "InvalidInput"
  | "Storage"
  | "Internal";

export type CommandError = {
  code: CommandErrorCode;
  message: string;
  retryable: boolean;
  details: unknown | null;
};

const commandErrorCodes: readonly CommandErrorCode[] = [
  "NotInitialized",
  "AlreadyInitialized",
  "Authentication",
  "Network",
  "WrongKey",
  "RemoteConflict",
  "RateLimited",
  "RemoteServer",
  "CorruptedData",
  "InvalidInput",
  "Storage",
  "Internal"
];

function isCommandErrorCode(code: unknown): code is CommandErrorCode {
  return typeof code === "string" && commandErrorCodes.includes(code as CommandErrorCode);
}

export function commandError(error: unknown): CommandError | null {
  if (!error || typeof error !== "object") return null;
  const candidate = error as Partial<CommandError>;
  if (
    !isCommandErrorCode(candidate.code) ||
    typeof candidate.message !== "string" ||
    typeof candidate.retryable !== "boolean"
  ) {
    return null;
  }
  return {
    code: candidate.code,
    message: candidate.message,
    retryable: candidate.retryable,
    details: candidate.details ?? null
  };
}

export function errorText(error: unknown) {
  return commandError(error)?.message ?? (error instanceof Error ? error.message : String(error));
}

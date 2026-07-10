export type SetupWarning = {
  code: "ReconnectRequired";
  message: string;
};

export function setupWarningMessage(warning: SetupWarning | null): string | null {
  if (warning?.code !== "ReconnectRequired" || typeof warning.message !== "string") {
    return null;
  }
  return warning.message;
}

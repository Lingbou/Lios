export type RecoveryKeyStatus = {
  key_location?: string | null;
  backed_up: boolean;
  backup_location?: string | null;
};

export type RecoveryKeyVerification = {
  format_valid: boolean;
  catalog_checked: boolean;
  checked_space?: {
    namespace: string;
    dataset: string;
    endpoint: string;
  } | null;
};

export function conciseRecoveryKeyPath(value?: string | null) {
  if (!value) return "-";
  const normalized = value.replace(/^\\\\\?\\/, "").replace(/\\/g, "/");
  const marker = "/.lios/";
  const markerIndex = normalized.toLowerCase().lastIndexOf(marker);
  if (markerIndex >= 0) return `~${normalized.slice(markerIndex)}`;
  const parts = normalized.split("/").filter(Boolean);
  return parts.slice(-2).join("/") || normalized;
}

export function recoveryKeyBackupText(status?: RecoveryKeyStatus | null) {
  if (!status?.backed_up) return "尚未备份";
  const location = conciseRecoveryKeyPath(status.backup_location);
  const fileName = location.split("/").filter(Boolean).pop();
  return fileName ? `已备份 · ${fileName}` : "已备份";
}

export function recoveryKeyConfirmationText(verification: RecoveryKeyVerification) {
  const space = verification.checked_space;
  if (verification.catalog_checked && space) {
    return `已使用 ${space.namespace}/${space.dataset} 的 Catalog 验证此恢复密钥。确认后将绑定所选外部密钥文件。`;
  }
  if (space) {
    return `${space.namespace}/${space.dataset} 尚未初始化，因此仅检查了密钥文件格式。确认后将绑定所选外部密钥文件。`;
  }
  return "当前未配置可验证的空间，因此仅检查了密钥文件格式。确认后将绑定所选外部密钥文件。";
}

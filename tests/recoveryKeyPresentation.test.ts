import assert from "node:assert/strict";
import test from "node:test";

import {
  conciseRecoveryKeyPath,
  recoveryKeyBackupText,
  recoveryKeyConfirmationText
} from "../src/recoveryKeyPresentation.ts";

test("recovery key status uses concise locations and rejects stale backup state", () => {
  assert.equal(
    conciseRecoveryKeyPath("C:\\Users\\novix\\.lios\\recovery.key"),
    "~/.lios/recovery.key"
  );
  assert.equal(
    recoveryKeyBackupText({
      key_location: "C:\\Users\\novix\\.lios\\recovery.key",
      backed_up: true,
      backup_location: "D:\\Backups\\lios-recovery.key"
    }),
    "已备份 · lios-recovery.key"
  );
  assert.equal(
    recoveryKeyBackupText({
      key_location: "C:\\Users\\novix\\.lios\\recovery.key",
      backed_up: false,
      backup_location: "D:\\Backups\\missing.key"
    }),
    "尚未备份"
  );
});

test("import confirmation distinguishes catalog verification from format-only checks", () => {
  assert.match(
    recoveryKeyConfirmationText({
      format_valid: true,
      catalog_checked: true,
      checked_space: {
        namespace: "novix",
        dataset: "archive",
        endpoint: "https://modelscope.cn"
      }
    }),
    /novix\/archive.*Catalog/
  );
  assert.match(
    recoveryKeyConfirmationText({
      format_valid: true,
      catalog_checked: false,
      checked_space: null
    }),
    /仅检查了密钥文件格式/
  );
});

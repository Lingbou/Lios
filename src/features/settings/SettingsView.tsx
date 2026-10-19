import React from "react";
import {
  Download,
  HardDrive,
  KeyRound,
  RefreshCw,
  ShieldCheck,
  Trash2,
  UploadCloud
} from "lucide-react";
import type {
  CacheCleanupReport,
  CatalogRebuildDialog,
  ModelScopeUserSummary,
  RecoveryKeyImportDialog,
  Snapshot,
  SpaceSummary
} from "../../appTypes";
import {
  displayPath,
  formatCacheBytes
} from "../catalog/catalogPresentation";
import {
  conciseRecoveryKeyPath,
  recoveryKeyBackupText
} from "../../recoveryKeyPresentation";
import { AboutSection } from "./AboutSection";
import { ConnectionSection, EndpointSection } from "./ConnectionSettings";

export interface SettingsViewProps {
  hasToken: boolean;
  modelscopeUser: ModelScopeUserSummary | null;
  token: string;
  onTokenChange: (val: string) => void;
  onConnect: () => void;
  onDisconnect: () => void;
  manualEndpoint: string;
  onEndpointChange: (val: string) => void;
  onSaveEndpoint: () => void;
  snapshot: Snapshot | null;
  busy: boolean;
  recoveryKeyImport: RecoveryKeyImportDialog | null;
  onExportRecoveryKey: () => void;
  onSelectRecoveryKeyForImport: () => void;
  selectedSpace: SpaceSummary | null;
  onVerifySpace: (full: boolean) => void;
  onOpenCatalogRebuildDialog: () => void;
  rebuildDialog: CatalogRebuildDialog | null;
  rebuildTaskActive: boolean;
  activeTasks: number;
  onChangeChunkSize: (chunkSizeBytes: number) => void;
  cacheCleanup: CacheCleanupReport | null;
  onCleanupLocalCache: () => void;
  loadAppVersion: () => Promise<string>;
}

export const SettingsView: React.FC<SettingsViewProps> = ({
  hasToken,
  modelscopeUser,
  token,
  onTokenChange,
  onConnect,
  onDisconnect,
  manualEndpoint,
  onEndpointChange,
  onSaveEndpoint,
  snapshot,
  busy,
  recoveryKeyImport,
  onExportRecoveryKey,
  onSelectRecoveryKeyForImport,
  selectedSpace,
  onVerifySpace,
  onOpenCatalogRebuildDialog,
  rebuildDialog,
  rebuildTaskActive,
  activeTasks,
  onChangeChunkSize,
  cacheCleanup,
  onCleanupLocalCache,
  loadAppVersion
}) => {
  return (
    <section className="settingsPage">
      <div className="settingsPageHeader">
        <div>
          <h2>设置</h2>
          <span>应用、安全与本地状态</span>
        </div>
      </div>
      <div className="settingsBlocks">
        <ConnectionSection
          hasToken={hasToken}
          username={modelscopeUser?.username}
          token={token}
          onTokenChange={onTokenChange}
          onConnect={onConnect}
          onDisconnect={onDisconnect}
          busy={busy}
        />

        <EndpointSection
          endpoint={manualEndpoint}
          onEndpointChange={onEndpointChange}
          onSave={onSaveEndpoint}
          busy={busy}
        />

        <div className="settingsBlock recoveryKeyBlock">
          <div className="settingsHeaderRow">
            <div>
              <h2>恢复密钥</h2>
              <p>用于打开已加密的 Lios 空间</p>
            </div>
            <div className="settingsActions">
              <button
                onClick={onExportRecoveryKey}
                disabled={!snapshot?.recovery_key.key_location || busy}
              >
                <Download aria-hidden />
                导出备份
              </button>
              <button
                onClick={onSelectRecoveryKeyForImport}
                disabled={busy || recoveryKeyImport !== null}
              >
                <UploadCloud aria-hidden />
                导入密钥
              </button>
            </div>
          </div>
          <div className="recoveryKeySummary">
            <div className="recoveryKeyLocation">
              <KeyRound aria-hidden />
              <span>
                <small>当前密钥</small>
                <strong title={snapshot?.recovery_key.key_location ?? undefined}>
                  {conciseRecoveryKeyPath(snapshot?.recovery_key.key_location)}
                </strong>
              </span>
            </div>
            <span
              className={`backupStatus ${snapshot?.recovery_key.backed_up ? "ready" : "missing"}`}
              title={snapshot?.recovery_key.backup_location ?? undefined}
            >
              {recoveryKeyBackupText(snapshot?.recovery_key)}
            </span>
          </div>
        </div>

        <div className="settingsBlock">
          <div className="settingsHeaderRow">
            <div>
              <h2>空间检查</h2>
              <p>
                {selectedSpace
                  ? `${selectedSpace.namespace}/${selectedSpace.dataset}`
                  : "未选择空间"}
              </p>
            </div>
            <div className="settingsActions">
              <button
                onClick={() => onVerifySpace(false)}
                disabled={!selectedSpace || busy}
              >
                <ShieldCheck aria-hidden />
                快速检查
              </button>
              <button
                onClick={() => onVerifySpace(true)}
                disabled={!selectedSpace || busy}
              >
                <HardDrive aria-hidden />
                完整检查
              </button>
              <button
                onClick={onOpenCatalogRebuildDialog}
                disabled={
                  !selectedSpace || busy || rebuildDialog !== null || rebuildTaskActive
                }
              >
                <RefreshCw aria-hidden />
                重建 Catalog
              </button>
            </div>
          </div>
        </div>

        <div className="settingsBlock">
          <div className="settingsHeaderRow">
            <div>
              <h2>传输分块</h2>
              <p>设置大文件上传的分块大小（弱网环境下建议使用较小分块以提升断点重传成功率）</p>
            </div>
            <select
              value={snapshot?.config.chunk_size ?? 134217728}
              onChange={(event) => onChangeChunkSize(Number(event.target.value))}
              disabled={busy || activeTasks > 0}
            >
              <option value={16777216}>16 MB (弱网快速重传)</option>
              <option value={33554432}>32 MB (推荐 / 均衡)</option>
              <option value={67108864}>64 MB (高速网络)</option>
              <option value={134217728}>128 MB (超大单块)</option>
            </select>
          </div>
        </div>

        <div className="settingsBlock">
          <div className="settingsHeaderRow">
            <div>
              <h2>本地状态</h2>
            </div>
            <button onClick={onCleanupLocalCache} disabled={busy || activeTasks > 0}>
              <Trash2 aria-hidden />
              清理缓存
            </button>
          </div>
          {cacheCleanup && (
            <div className="cleanupResult">
              已清理 {cacheCleanup.files_removed} 个文件、{cacheCleanup.dirs_removed} 个空目录，释放{" "}
              {formatCacheBytes(cacheCleanup.bytes_removed)}
            </div>
          )}
          <dl className="pathGrid">
            <div>
              <dt>Config</dt>
              <dd>{displayPath(snapshot?.paths.config)}</dd>
            </div>
            <div>
              <dt>Database</dt>
              <dd>{displayPath(snapshot?.paths.database)}</dd>
            </div>
            <div>
              <dt>Staging</dt>
              <dd>{displayPath(snapshot?.paths.staging)}</dd>
            </div>
            <div>
              <dt>Logs</dt>
              <dd title={snapshot?.paths.logs ?? undefined}>
                {conciseRecoveryKeyPath(snapshot?.paths.logs)}
              </dd>
            </div>
          </dl>
        </div>

        <AboutSection loadVersion={loadAppVersion} />
      </div>
    </section>
  );
};

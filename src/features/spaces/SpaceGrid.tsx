import { AlertTriangle, ChevronRight, Cloud, HardDrive, Plus, RefreshCw, Settings } from "lucide-react";
import type { SpaceSummary } from "../../appTypes.ts";

export interface SpaceGridProps {
  accountName: string;
  hasToken: boolean;
  spaces: SpaceSummary[];
  activeSpace: SpaceSummary | null;
  query: string;
  busy: boolean;
  onRefresh: () => void;
  onCreateSpace: () => void;
  onSelectSpace: (space: SpaceSummary) => void;
  onOpenSettings: () => void;
}

export function SpaceGrid({
  accountName,
  hasToken,
  spaces,
  activeSpace,
  query,
  busy,
  onRefresh,
  onCreateSpace,
  onSelectSpace,
  onOpenSettings
}: SpaceGridProps) {
  const isSearching = Boolean(query.trim());

  return (
    <section className="accountSpacesPage">
      <div className="accountSpacesHeader">
        <div>
          <h2>{accountName}</h2>
          <span>{hasToken ? "ModelScope" : "未连接"}</span>
        </div>
        <div className="toolbar">
          <button type="button" onClick={onRefresh} disabled={!hasToken || busy}>
            <RefreshCw aria-hidden />
            <span>刷新</span>
          </button>
          <button
            type="button"
            className="primary"
            onClick={onCreateSpace}
            disabled={!hasToken || busy}
          >
            <Plus aria-hidden />
            <span>创建空间</span>
          </button>
        </div>
      </div>

      <section className="spaceSurface">
        {!hasToken ? (
          <div className="emptyDrive">
            <Cloud aria-hidden />
            <h2>连接 ModelScope</h2>
            <button type="button" className="primary" onClick={onOpenSettings}>
              <Settings aria-hidden />
              <span>设置令牌</span>
            </button>
          </div>
        ) : spaces.length === 0 ? (
          <div className="emptyDrive">
            <HardDrive aria-hidden />
            <h2>{isSearching ? "没有匹配空间" : "创建一个空间"}</h2>
            {!isSearching && (
              <button
                type="button"
                className="primary"
                onClick={onCreateSpace}
                disabled={busy}
              >
                <Plus aria-hidden />
                <span>创建空间</span>
              </button>
            )}
          </div>
        ) : (
          <div className="spaceGrid">
            {spaces.map((space) => {
              const active =
                activeSpace?.namespace === space.namespace &&
                activeSpace?.dataset === space.dataset &&
                activeSpace?.endpoint === space.endpoint;
              const hasNonAscii = /[^\x00-\x7F]/.test(space.dataset);

              return (
                <button
                  type="button"
                  className={`spaceCard ${active ? "active" : ""} ${hasNonAscii ? "unsupportedCard" : ""}`}
                  key={`${space.endpoint}/${space.namespace}/${space.dataset}`}
                  onClick={() => {
                    if (hasNonAscii) {
                      window.alert(
                        `数据集「${space.dataset}」名称包含非 ASCII 字符（如中文）。\n\n由于 ModelScope 平台的底层 Git 服务对中文路径缺乏良好支持，极易导致空分支 404 或删除接口 500 崩溃，Lios 暂不支持将其关联为空间驱动器。\n\n建议在客户端或网页端创建纯英文标识的数据集。`
                      );
                      return;
                    }
                    onSelectSpace(space);
                  }}
                  title={
                    hasNonAscii
                      ? "名称包含非 ASCII 字符（如中文），暂不支持关联"
                      : `${space.namespace}/${space.dataset}`
                  }
                >
                  <HardDrive aria-hidden />
                  <span>
                    <strong>{space.dataset}</strong>
                    <small>
                      {space.namespace}
                      {hasNonAscii && " · 不支持中文"}
                    </small>
                  </span>
                  {hasNonAscii ? (
                    <AlertTriangle className="unsupportedIcon" aria-hidden />
                  ) : (
                    <ChevronRight aria-hidden />
                  )}
                </button>
              );
            })}
          </div>
        )}
      </section>
    </section>
  );
}

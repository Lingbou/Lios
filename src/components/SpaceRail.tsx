import { ArrowUpDown, HardDrive, KeyRound, Plus, Settings } from "lucide-react";
import type { SpaceSummary } from "../appTypes.ts";

type SpaceRailProps = {
  hasToken: boolean;
  username?: string | null;
  spaces: SpaceSummary[];
  activeSpaceName?: string | null;
  view: "spaces" | "drive" | "settings";
  taskDrawerCollapsed: boolean;
  activeTasks: number;
  onSelectAccount: () => void;
  onCreateSpace: () => void;
  onSelectSpace: (space: SpaceSummary) => void;
  onToggleTaskDrawer: () => void;
  onToggleSettings: () => void;
};

export function SpaceRail({
  hasToken,
  username,
  spaces,
  activeSpaceName,
  view,
  taskDrawerCollapsed,
  activeTasks,
  onSelectAccount,
  onCreateSpace,
  onSelectSpace,
  onToggleTaskDrawer,
  onToggleSettings
}: SpaceRailProps) {
  return (
    <aside className="spaceRail">
      <div className="railHeader">
        <div className="accountSection">
          <button
            className={`accountItem ${hasToken ? "active" : "empty"}`}
            onClick={onSelectAccount}
            title="查看所有空间概览"
          >
            <KeyRound aria-hidden />
            <span>
              <strong>{username ?? "未连接账号"}</strong>
              <small>{hasToken ? "ModelScope" : "未连接"}</small>
            </span>
          </button>
        </div>
      </div>

      <div className="railSpacesSection">
        <div className="railSectionHeader">
          <span className="sectionLabel">已挂载空间</span>
          <button
            type="button"
            className="railAddSpaceBtn"
            onClick={onCreateSpace}
            title="新建空间"
            aria-label="新建空间"
          >
            <Plus aria-hidden />
          </button>
        </div>

        <div className="railSpaceList" role="list">
          {spaces.map((space) => {
            const isActive = view === "drive" && activeSpaceName === space.space_name;
            const displayName = space.title || space.dataset || space.space_name;
            return (
              <button
                key={space.space_name}
                type="button"
                className={`railSpaceItem ${isActive ? "active" : ""}`}
                onClick={() => onSelectSpace(space)}
                title={displayName}
              >
                <HardDrive aria-hidden />
                <span className="railSpaceName">{displayName}</span>
              </button>
            );
          })}
          {spaces.length === 0 && (
            <div className="railEmptySpaces">
              <span>暂无已挂载空间</span>
              <button type="button" className="railEmptyCreateBtn" onClick={onCreateSpace}>
                <Plus aria-hidden /> 新建空间
              </button>
            </div>
          )}
        </div>
      </div>

      <div className="railFooter">
        <button
          type="button"
          className={view !== "settings" && !taskDrawerCollapsed ? "active" : ""}
          onClick={onToggleTaskDrawer}
          title="传输任务概览"
        >
          <ArrowUpDown aria-hidden />
          <span>传输任务</span>
          {activeTasks > 0 && <span className="railTaskBadge">{activeTasks}</span>}
        </button>
        <button
          type="button"
          className={view === "settings" ? "active" : ""}
          onClick={onToggleSettings}
          title="设置"
        >
          <Settings aria-hidden />
          <span>设置</span>
        </button>
      </div>
    </aside>
  );
}

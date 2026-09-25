import { LogOut, ShieldCheck } from "lucide-react";

export type ConnectionSectionProps = {
  hasToken: boolean;
  username?: string | null;
  token: string;
  onTokenChange: (token: string) => void;
  onConnect: () => void;
  onDisconnect: () => void;
  busy: boolean;
};

export function ConnectionSection({
  hasToken,
  username,
  token,
  onTokenChange,
  onConnect,
  onDisconnect,
  busy
}: ConnectionSectionProps) {
  const isTokenEmpty = !token.trim();

  return (
    <div className="settingsBlock">
      <div className="settingsHeaderRow">
        <div>
          <h2>连接</h2>
          <p>
            {hasToken
              ? username
                ? `已连接 ${username}`
                : "已连接 (凭据已保存)"
              : "输入访问凭证连接 ModelScope 账号"}
          </p>
        </div>
        {hasToken && (
          <div className="settingsActions">
            <button
              type="button"
              onClick={onDisconnect}
              disabled={busy}
              title="断开连接并清除本地保存的凭据"
            >
              <LogOut aria-hidden />
              断开连接
            </button>
          </div>
        )}
      </div>
      <div className="tokenRow">
        <input
          type="password"
          value={token}
          onChange={(event) => onTokenChange(event.target.value)}
          placeholder={hasToken ? "输入新的 Access Token 以更新凭据" : "ModelScope access token"}
          autoComplete="off"
        />
        <button
          type="button"
          className="primary"
          onClick={onConnect}
          disabled={isTokenEmpty || busy}
        >
          <ShieldCheck aria-hidden />
          {hasToken ? "更新凭据" : "连接"}
        </button>
      </div>
    </div>
  );
}

export type EndpointSectionProps = {
  endpoint: string;
  onEndpointChange: (endpoint: string) => void;
  onSave: () => void;
  busy: boolean;
};

export function EndpointSection({
  endpoint,
  onEndpointChange,
  onSave,
  busy
}: EndpointSectionProps) {
  const isEndpointEmpty = !endpoint.trim();

  return (
    <div className="settingsBlock">
      <div className="settingsHeaderRow">
        <div>
          <h2>服务地址</h2>
          <p>ModelScope 平台服务与 API 端点</p>
        </div>
      </div>
      <div className="tokenRow">
        <input
          className="endpointInput"
          value={endpoint}
          onChange={(event) => onEndpointChange(event.target.value)}
          placeholder="https://modelscope.cn"
        />
        <button
          type="button"
          onClick={onSave}
          disabled={isEndpointEmpty || busy}
        >
          <ShieldCheck aria-hidden />
          保存端点
        </button>
      </div>
    </div>
  );
}

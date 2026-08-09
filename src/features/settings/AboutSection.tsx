import { useEffect, useState } from "react";
import liosPetalMark from "../../assets/lios-petal-mark.svg";

type AboutSectionProps = {
  loadVersion: () => Promise<string>;
};

export function AboutSection({ loadVersion }: AboutSectionProps) {
  const [version, setVersion] = useState<string | null>(null);
  const [versionUnavailable, setVersionUnavailable] = useState(false);

  useEffect(() => {
    let active = true;

    setVersion(null);
    setVersionUnavailable(false);
    loadVersion()
      .then((nextVersion) => {
        if (active) setVersion(nextVersion);
      })
      .catch(() => {
        if (active) setVersionUnavailable(true);
      });

    return () => {
      active = false;
    };
  }, [loadVersion]);

  const versionLabel = versionUnavailable
    ? "版本未知"
    : version === null
      ? "读取中"
      : version === "开发预览"
        ? version
        : `v${version}`;

  return (
    <section className="settingsBlock aboutBlock" aria-labelledby="aboutLiosTitle">
      <div className="aboutIdentity">
        <div className="aboutMark" aria-hidden>
          <img src={liosPetalMark} alt="" />
        </div>
        <div className="aboutCopy">
          <h2 id="aboutLiosTitle">关于 Lios</h2>
          <p>加密的 ModelScope 逻辑云盘</p>
        </div>
      </div>

      <dl className="aboutMeta">
        <div className="aboutVersionMeta">
          <dt>版本</dt>
          <dd aria-live="polite">{versionLabel}</dd>
        </div>
        <div>
          <dt>许可证</dt>
          <dd>MIT</dd>
        </div>
        <div>
          <dt>组件</dt>
          <dd>Desktop 与 CLI 使用同一版本号</dd>
        </div>
      </dl>
    </section>
  );
}

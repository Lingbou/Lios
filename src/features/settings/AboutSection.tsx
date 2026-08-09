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
          <p>干翻百度网盘</p>
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
          <dt>GitHub</dt>
          <dd title="https://github.com/Lingbou">Lingbou</dd>
        </div>
      </dl>
    </section>
  );
}

import { Minus, Square, X } from "lucide-react";
import type { MouseEvent } from "react";
import liosPetalMark from "../assets/lios-petal-mark.svg";

type TitlebarProps = {
  onDrag: (e: MouseEvent<HTMLElement>) => void | Promise<void>;
  onMinimize: () => void | Promise<void>;
  onToggleMaximize: () => void | Promise<void>;
  onClose: () => void | Promise<void>;
};

export function Titlebar({
  onDrag,
  onMinimize,
  onToggleMaximize,
  onClose
}: TitlebarProps) {
  return (
    <header className="windowTitlebar">
      <div className="windowTitle" data-tauri-drag-region onMouseDown={onDrag}>
        <img src={liosPetalMark} alt="" />
        <span>Lios</span>
      </div>
      <div className="windowDragRegion" data-tauri-drag-region onMouseDown={onDrag} />
      <div className="windowControls">
        <button onClick={onMinimize} title="最小化" aria-label="最小化">
          <Minus aria-hidden />
        </button>
        <button onClick={onToggleMaximize} title="最大化" aria-label="最大化">
          <Square aria-hidden />
        </button>
        <button className="closeWindow" onClick={onClose} title="关闭" aria-label="关闭">
          <X aria-hidden />
        </button>
      </div>
    </header>
  );
}

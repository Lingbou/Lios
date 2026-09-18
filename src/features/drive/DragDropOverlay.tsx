import { UploadCloud } from "lucide-react";

interface DragDropOverlayProps {
  isDragging: boolean;
  targetDirName?: string;
}

export function DragDropOverlay({ isDragging, targetDirName }: DragDropOverlayProps) {
  if (!isDragging) return null;

  return (
    <div className="dragDropOverlay" aria-hidden>
      <div className="dragDropCard">
        <UploadCloud className="dragDropIcon" aria-hidden />
        <strong>释放以开始上传</strong>
        <span>将上传至 {targetDirName ? `“${targetDirName}”` : "当前目录"}</span>
      </div>
    </div>
  );
}

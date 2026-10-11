import { useRef, useState } from "react";
import type { DriveItem } from "../../appTypes.ts";
import type { SpaceSummary } from "../../appTypes.ts";
import type { FilePreviewContent } from "./FilePreviewModal.tsx";
import { errorText } from "../../commandError.ts";
import { appInvoke } from "../../runtime.ts";

export function useFilePreview(activeSpace: SpaceSummary | null) {
  const [previewOpen, setPreviewOpen] = useState(false);
  const [previewItem, setPreviewItem] = useState<DriveItem | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewContent, setPreviewContent] = useState<FilePreviewContent | null>(null);
  const previewRequest = useRef(0);

  async function openFilePreview(item: DriveItem) {
    if (!activeSpace) return;
    const requestId = ++previewRequest.current;
    const spaceName = activeSpace.space_name;
    setPreviewItem(item);
    setPreviewOpen(true);
    setPreviewLoading(true);
    setPreviewError(null);
    setPreviewContent(null);
    try {
      const res = await appInvoke<{
        name: string;
        size: number;
        mime_type: string;
        is_text: boolean;
        text?: string;
        data_url?: string;
      }>("preview_file_node", {
        spaceName,
        nodeId: item.id
      });
      if (requestId !== previewRequest.current) return;
      setPreviewContent({
        isText: res.is_text,
        text: res.text,
        dataUrl: res.data_url
      });
    } catch (err) {
      if (requestId !== previewRequest.current) return;
      setPreviewError(errorText(err));
    } finally {
      if (requestId !== previewRequest.current) return;
      setPreviewLoading(false);
    }
  }

  function closePreview() {
    previewRequest.current += 1;
    setPreviewOpen(false);
    setPreviewItem(null);
  }

  return {
    previewOpen,
    previewItem,
    previewLoading,
    previewError,
    previewContent,
    openFilePreview,
    closePreview
  };
}
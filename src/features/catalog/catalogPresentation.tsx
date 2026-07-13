import { File, Folder } from "lucide-react";
import type {
  CatalogTreeNode,
  DriveItem,
  RepoConfig
} from "../../appTypes.ts";

export function formatBytes(bytes: number) {
  if (!bytes) return "0 B";
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = units.shift() ?? "KB";
  while (value >= 1024 && units.length > 0) {
    value /= 1024;
    unit = units.shift() ?? unit;
  }
  return `${value.toFixed(value >= 10 ? 1 : 2)} ${unit}`;
}

export function formatDate(value?: string | null) {
  if (!value) return "-";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "-";
  return date.toLocaleString();
}

export function displayPath(value?: string | null) {
  if (!value) return "-";
  return value.replace(/^.*[\\/]/, "");
}

export function formatCacheBytes(bytes: number) {
  return bytes <= 0 ? "0 B" : formatBytes(bytes);
}

export function findNode(
  node: CatalogTreeNode | null,
  id: string | null
): CatalogTreeNode | null {
  if (!node || !id) return node;
  if (node.id === id) return node;
  if (node.kind.type === "Directory") {
    for (const child of node.kind.children) {
      const found = findNode(child, id);
      if (found) return found;
    }
  }
  return null;
}

export function breadcrumb(
  node: CatalogTreeNode | null,
  targetId: string | null
): CatalogTreeNode[] {
  if (!node) return [];
  if (!targetId || node.id === targetId) return [node];
  if (node.kind.type === "Directory") {
    for (const child of node.kind.children) {
      const nested = breadcrumb(child, targetId);
      if (nested.length > 0) return [node, ...nested];
    }
  }
  return [];
}

export function treeToDriveItem(node: CatalogTreeNode): DriveItem {
  return {
    id: node.id,
    name: node.name,
    kind: node.kind.type === "Directory" ? "Directory" : "File",
    size: node.kind.type === "File" ? node.kind.original_size : 0,
    updated_at: node.updated_at,
    children_count:
      node.kind.type === "Directory" ? node.kind.children.length : 0
  };
}

export function CatalogRecoveryTree({ node }: { node: CatalogTreeNode }) {
  const kind = node.kind;
  return (
    <li>
      <div className="rebuildTreeRow">
        {kind.type === "Directory" ? <Folder aria-hidden /> : <File aria-hidden />}
        <span>{node.name || "根目录"}</span>
        {kind.type === "File" && <small>{formatBytes(kind.original_size)}</small>}
      </div>
      {kind.type === "Directory" && kind.children.length > 0 && (
        <ul>
          {kind.children.map((child) => (
            <CatalogRecoveryTree node={child} key={child.id} />
          ))}
        </ul>
      )}
    </li>
  );
}

export function sameSpace(
  left: RepoConfig | null | undefined,
  right: RepoConfig | null | undefined
) {
  return Boolean(
    left &&
      right &&
      left.namespace === right.namespace &&
      left.dataset === right.dataset &&
      left.endpoint === right.endpoint
  );
}

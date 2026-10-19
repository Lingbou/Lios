import React from "react";
import type { ReactNode } from "react";
import type {
  CatalogRebuildDialog,
  ConflictAction,
  DriveItem,
  RecoveryKeyImportDialog,
  UploadConflict
} from "../appTypes";
import type { ContextMenuState } from "../features/drive/driveTypes";
import type { FilePreviewContent } from "../features/drive/FilePreviewModal";
import { RebuildCatalogModal } from "../features/catalog/RebuildCatalogModal";
import { ConflictModal } from "../features/drive/ConflictModal";
import { ContextMenu } from "../features/drive/ContextMenu";
import { FilePreviewModal } from "../features/drive/FilePreviewModal";
import { ImportKeyModal } from "../features/recoveryKey/ImportKeyModal";
import { CreateSpaceModal } from "../features/spaces/CreateSpaceModal";
import { ConfirmModal } from "./ConfirmModal";
import { InputModal } from "./InputModal";

export interface AppModalsProps {
  contextMenu: ContextMenuState;
  onCloseContextMenu: () => void;
  selectedCount: number;
  selectedIds: Set<string>;
  onOpenItem: (item: DriveItem) => void;
  onPreviewItem: (item: DriveItem) => void;
  onDownload: (target?: DriveItem | string[]) => void;
  onRename: (target?: DriveItem | string) => void;
  onDelete: (target?: DriveItem | string[]) => void;
  onUploadFiles: () => void;
  onUploadFolder: () => void;
  onNewFolder: () => void;
  onRefresh: () => void;
  onSelectAll: () => void;

  rebuildDialog: CatalogRebuildDialog | null;
  rebuildTaskActive: boolean;
  onCloseCatalogRebuildDialog: () => void;
  onReloadCatalogRebuildPreview: () => void;
  onConfirmCatalogRebuild: () => void;

  recoveryKeyImport: RecoveryKeyImportDialog | null;
  onCloseRecoveryKeyImport: () => void;
  onConfirmRecoveryKeyImport: () => void;

  conflicts: UploadConflict[];
  conflictActions: Record<string, ConflictAction>;
  onSetConflictAction: (path: string, action: ConflictAction) => void;
  onSetAllConflictActions: (action: ConflictAction) => void;
  onCancelConflicts: () => void;
  onConfirmConflicts: () => void;

  createSpaceOpen: boolean;
  newSpaceName: string;
  newSpaceSlug: string;
  createSpaceError: string;
  busy: boolean;
  onChangeSpaceName: (name: string) => void;
  onChangeSpaceSlug: (slug: string) => void;
  onCloseCreateSpace: () => void;
  onSubmitCreateSpace: () => void;

  previewOpen: boolean;
  previewItem: DriveItem | null;
  previewLoading: boolean;
  previewError: string | null;
  previewContent: FilePreviewContent | null;
  hasPrevPreview: boolean;
  hasNextPreview: boolean;
  onPrevPreview: () => void;
  onNextPreview: () => void;
  onClosePreview: () => void;
  onDownloadPreview: (item: DriveItem) => void;

  inputModalState: {
    open: boolean;
    title: string;
    description?: string;
    inputLabel?: string;
    placeholder?: string;
    defaultValue?: string;
    confirmText?: string;
    validate?: (value: string) => string | null | undefined;
    onSubmit: (value: string) => void | Promise<void>;
  };
  onCloseInputModal: () => void;

  confirmModalState: {
    open: boolean;
    title: string;
    description?: string;
    details?: ReactNode;
    confirmText?: string;
    danger?: boolean;
    hideCancel?: boolean;
    onConfirm: () => void | Promise<void>;
  };
  onCloseConfirmModal: () => void;
}

export const AppModals: React.FC<AppModalsProps> = ({
  contextMenu,
  onCloseContextMenu,
  selectedCount,
  selectedIds,
  onOpenItem,
  onPreviewItem,
  onDownload,
  onRename,
  onDelete,
  onUploadFiles,
  onUploadFolder,
  onNewFolder,
  onRefresh,
  onSelectAll,
  rebuildDialog,
  rebuildTaskActive,
  onCloseCatalogRebuildDialog,
  onReloadCatalogRebuildPreview,
  onConfirmCatalogRebuild,
  recoveryKeyImport,
  onCloseRecoveryKeyImport,
  onConfirmRecoveryKeyImport,
  conflicts,
  conflictActions,
  onSetConflictAction,
  onSetAllConflictActions,
  onCancelConflicts,
  onConfirmConflicts,
  createSpaceOpen,
  newSpaceName,
  newSpaceSlug,
  createSpaceError,
  busy,
  onChangeSpaceName,
  onChangeSpaceSlug,
  onCloseCreateSpace,
  onSubmitCreateSpace,
  previewOpen,
  previewItem,
  previewLoading,
  previewError,
  previewContent,
  hasPrevPreview,
  hasNextPreview,
  onPrevPreview,
  onNextPreview,
  onClosePreview,
  onDownloadPreview,
  inputModalState,
  onCloseInputModal,
  confirmModalState,
  onCloseConfirmModal
}) => {
  return (
    <>
      <ContextMenu
        state={contextMenu}
        onClose={onCloseContextMenu}
        selectedCount={
          contextMenu.item != null && !selectedIds.has(contextMenu.item.id)
            ? 1
            : selectedCount
        }
        onOpenItem={onOpenItem}
        onPreviewItem={onPreviewItem}
        onDownload={onDownload}
        onRename={onRename}
        onDelete={onDelete}
        onUploadFiles={onUploadFiles}
        onUploadFolder={onUploadFolder}
        onNewFolder={onNewFolder}
        onRefresh={onRefresh}
        onSelectAll={onSelectAll}
      />

      <RebuildCatalogModal
        dialog={rebuildDialog}
        rebuildTaskActive={rebuildTaskActive}
        onClose={onCloseCatalogRebuildDialog}
        onReloadPreview={onReloadCatalogRebuildPreview}
        onConfirm={onConfirmCatalogRebuild}
      />

      <ImportKeyModal
        dialog={recoveryKeyImport}
        onClose={onCloseRecoveryKeyImport}
        onConfirm={onConfirmRecoveryKeyImport}
      />

      <ConflictModal
        conflicts={conflicts}
        conflictActions={conflictActions}
        onSetAction={onSetConflictAction}
        onSetAllActions={onSetAllConflictActions}
        onCancel={onCancelConflicts}
        onConfirm={onConfirmConflicts}
      />

      <CreateSpaceModal
        open={createSpaceOpen}
        name={newSpaceName}
        slug={newSpaceSlug}
        error={createSpaceError}
        busy={busy}
        onChangeName={onChangeSpaceName}
        onChangeSlug={onChangeSpaceSlug}
        onClose={onCloseCreateSpace}
        onSubmit={onSubmitCreateSpace}
      />

      <FilePreviewModal
        open={previewOpen}
        item={previewItem}
        loading={previewLoading}
        error={previewError}
        content={previewContent}
        hasPrev={hasPrevPreview}
        hasNext={hasNextPreview}
        onPrev={onPrevPreview}
        onNext={onNextPreview}
        onClose={onClosePreview}
        onDownload={onDownloadPreview}
      />

      <InputModal
        open={inputModalState.open}
        title={inputModalState.title}
        description={inputModalState.description}
        inputLabel={inputModalState.inputLabel}
        placeholder={inputModalState.placeholder}
        defaultValue={inputModalState.defaultValue}
        confirmText={inputModalState.confirmText}
        validate={inputModalState.validate}
        onClose={onCloseInputModal}
        onSubmit={inputModalState.onSubmit}
      />

      <ConfirmModal
        open={confirmModalState.open}
        title={confirmModalState.title}
        description={confirmModalState.description}
        details={confirmModalState.details}
        confirmText={confirmModalState.confirmText}
        danger={confirmModalState.danger}
        hideCancel={confirmModalState.hideCancel}
        onClose={onCloseConfirmModal}
        onConfirm={confirmModalState.onConfirm}
      />
    </>
  );
};

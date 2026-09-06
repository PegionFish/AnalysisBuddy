/** ui/src/components/ConfirmDialog.tsx — 主题化确认对话框（破坏性操作统一封装，P0）。
 *  焦点管理：打开时聚焦取消键（破坏性操作默认安全项），Escape/遮罩点击 = 取消，
 *  关闭后焦点归还唤起元素。仅用 --ab-* 语义变量，深浅主题自适应。 */

import { useEffect, useRef } from 'react';
import './ConfirmDialog.css';

interface ConfirmDialogProps {
  open: boolean;
  title: string;
  body: string;
  confirmLabel: string;
  cancelLabel: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export default function ConfirmDialog({
  open,
  title,
  body,
  confirmLabel,
  cancelLabel,
  danger = false,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const cancelRef = useRef<HTMLButtonElement | null>(null);
  const restoreRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!open) return;
    restoreRef.current = document.activeElement as HTMLElement | null;
    cancelRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onCancel();
    };
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('keydown', onKey);
      // 焦点归还：对话框由 React 条件渲染卸载，此刻 activeElement 已回落 body。
      restoreRef.current?.focus?.();
      restoreRef.current = null;
    };
  }, [open, onCancel]);

  if (!open) return null;
  return (
    <div className="confirm-dialog__overlay" onClick={onCancel} data-testid="confirm-dialog">
      <div
        className="confirm-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="confirm-dialog__title"
        aria-describedby="confirm-dialog__body"
        onClick={(e) => e.stopPropagation()}
      >
        <h3 className="confirm-dialog__title" id="confirm-dialog__title">
          {title}
        </h3>
        <p className="confirm-dialog__body" id="confirm-dialog__body">
          {body}
        </p>
        <div className="confirm-dialog__actions">
          <button
            type="button"
            className="confirm-dialog__btn"
            ref={cancelRef}
            onClick={onCancel}
            data-testid="confirm-dialog-cancel"
          >
            {cancelLabel}
          </button>
          <button
            type="button"
            className={`confirm-dialog__btn confirm-dialog__btn--primary${danger ? ' confirm-dialog__btn--danger' : ''}`}
            onClick={onConfirm}
            data-testid="confirm-dialog-confirm"
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

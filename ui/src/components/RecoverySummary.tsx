/** ui/src/components/RecoverySummary.tsx — P1-03 会话恢复摘要（报告 P0-01 建议 4）。
 *  打开会话后（state.missing / state.reopenFailed 非空）固定在顶栏下方显示：
 *  「已恢复 X/Y 个文件」+ 缺失/重开失败汇总，可展开逐项原因（路径/原因/重试/
 *  复制诊断），可关闭。既有 missing-badge / reopen-failed-badge 徽标保持不变。 */

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { MissingFileEntry } from '../ipc/types';
import { reportError } from '../lib/globalErrors';
import { useSession } from '../state/session';
import './RecoverySummary.css';

/** 渲染恢复摘要。无失败条目时返回 null（不占位）。 */
export default function RecoverySummary() {
  const { state, actions } = useSession();
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const [dismissed, setDismissed] = useState(false);

  const failures: MissingFileEntry[] = [...state.missing, ...state.reopenFailed];
  if (failures.length === 0 || dismissed) return null;

  // Y=files+missing+reopenFailed 总数；X=files 中 ready 数（已恢复）。
  const total = state.files.length + failures.length;
  const recovered = state.files.filter((f) => f.status === 'ready').length;

  const reasonOf = (reason: MissingFileEntry['reason']): string => {
    if (reason === 'not_found') {
      return t('workbench.topbar.recovery_reason_not_found', { defaultValue: '文件缺失' });
    }
    if (reason === 'hash_mismatch') {
      return t('workbench.topbar.recovery_reason_hash_mismatch', { defaultValue: '内容已变更' });
    }
    return t('workbench.topbar.recovery_reason_reopen_failed', { defaultValue: '重新解析失败' });
  };

  /** 重试：对 reopen_failed / missing 条目一律按路径重新导入（missing 需文件仍在）。 */
  const retry = (entry: MissingFileEntry) => {
    void actions.importFiles([entry.path]).catch((e) => reportError(e, 'import_files'));
  };

  /** 复制诊断信息：无会话路径上下文时包含 路径+原因+时间戳。 */
  const copyDiagnostics = (entry: MissingFileEntry) => {
    const text = t('workbench.topbar.recovery_diagnostics_text', {
      path: entry.path,
      reason: reasonOf(entry.reason),
      timestamp: new Date().toISOString(),
      defaultValue: '文件：{{path}}\n原因：{{reason}}\n时间：{{timestamp}}',
    });
    if (!navigator.clipboard) return;
    void navigator.clipboard.writeText(text).catch(() => {});
  };

  return (
    <aside className="recovery-summary" data-testid="recovery-summary">
      <div className="recovery-summary__head" role="status" data-testid="recovery-summary-head">
        <span className="recovery-summary__count">
          {t('workbench.topbar.recovery_count', {
            recovered,
            total,
            defaultValue: '已恢复 {{recovered}}/{{total}} 个文件',
          })}
        </span>
        <span className="recovery-summary__failures">
          {t('workbench.topbar.recovery_failures', {
            missing: state.missing.length,
            failed: state.reopenFailed.length,
            defaultValue: '{{missing}} 个缺失，{{failed}} 个重开失败',
          })}
        </span>
        <button
          type="button"
          className="recovery-summary__btn"
          data-testid="recovery-toggle"
          onClick={() => setExpanded((v) => !v)}
        >
          {expanded
            ? t('workbench.topbar.recovery_hide_details', { defaultValue: '收起详情' })
            : t('workbench.topbar.recovery_show_details', { defaultValue: '查看详情' })}
        </button>
        <button
          type="button"
          className="recovery-summary__btn recovery-summary__close"
          data-testid="recovery-dismiss"
          aria-label={t('common.error.dismiss', { defaultValue: '关闭' })}
          onClick={() => setDismissed(true)}
        >
          ×
        </button>
      </div>

      {expanded && (
        <ul className="recovery-summary__list" role="alert" data-testid="recovery-failures">
          {failures.map((entry, i) => (
            <li key={`${entry.path}:${i}`} className="recovery-summary__item" data-testid="recovery-failure">
              <span className="recovery-summary__path" data-testid="recovery-path" title={entry.path}>
                {entry.path}
              </span>
              <span className="recovery-summary__reason" data-testid="recovery-reason">
                {reasonOf(entry.reason)}
              </span>
              <button
                type="button"
                className="recovery-summary__btn"
                data-testid="recovery-retry"
                title={
                  entry.reason === 'reopen_failed'
                    ? undefined
                    : t('workbench.topbar.recovery_retry_missing_hint', { defaultValue: '重试前请确认文件仍存在' })
                }
                onClick={() => retry(entry)}
              >
                {t('workbench.topbar.recovery_retry', { defaultValue: '重试' })}
              </button>
              <button
                type="button"
                className="recovery-summary__btn"
                data-testid="recovery-copy"
                onClick={() => copyDiagnostics(entry)}
              >
                {t('workbench.topbar.recovery_copy', { defaultValue: '复制诊断信息' })}
              </button>
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}

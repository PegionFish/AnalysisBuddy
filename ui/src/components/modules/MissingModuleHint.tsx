/** ui/src/components/modules/MissingModuleHint.tsx — 降级文件条目内的缺模块提示（P1-01/P1-02）。
 *  展示：当前解析器 + 置信度、推荐模块（指纹命中）或通用缺失提示、安装后新增能力，
 *  动作：添加模块（pickPluginZip + installPluginZip，成功后提示重新导入）/ 继续以通用方式读取（本次会话隐藏）。
 *  仅安装用户选择的本地 ZIP，不做任何静默联网下载。 */

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { pickPluginZip } from '../../ipc/real';
import type { ImportResult } from '../../ipc/types';
import { confidencePercent } from '../../lib/format';
import { useSession } from '../../state/session';
import { recommendModuleForFile } from './knownModules';
import './MissingModuleHint.css';

function errorText(e: unknown): { code: string; message: string } {
  if (typeof e === 'string') return { code: '', message: e };
  if (e && typeof e === 'object') {
    const obj = e as { code?: unknown; message?: unknown };
    return {
      code: typeof obj.code === 'string' ? obj.code : '',
      message: typeof obj.message === 'string' ? obj.message : '',
    };
  }
  return { code: '', message: '' };
}

export default function MissingModuleHint({
  entry,
  parserName,
  onDismiss,
}: {
  entry: ImportResult;
  /** 当前解析器显示名（matchedPluginName 解析后的 display_name 或 plugin_id）。 */
  parserName: string;
  /** 「继续以通用方式读取」：仅隐藏该文件本次会话的提示（不改变文件状态）。 */
  onDismiss: () => void;
}) {
  const { actions } = useSession();
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** 安装成功后提示重新导入（ZIP 安装不会自动重解析该文件）。 */
  const [installed, setInstalled] = useState(false);

  const recommendation = recommendModuleForFile(entry);
  const confidence = entry.matched_plugin?.confidence ?? 0;

  const onAddModule = async () => {
    setBusy(true);
    setError(null);
    try {
      const picked = await pickPluginZip();
      if (!picked) return; // 用户取消选择：静默
      await actions.installPluginZip(picked, false);
      setInstalled(true);
    } catch (e) {
      const { code, message } = errorText(e);
      setError(
        t(`common.error.${code}`, {
          defaultValue: message || t('common.error.internal'),
        }),
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="missing-module-hint" data-testid="missing-module-hint">
      <p className="missing-module-hint__parser">
        {t('workbench.files.hint.parser', {
          name: parserName,
          confidence: confidencePercent(confidence),
          defaultValue: '当前由 {{name}}（{{confidence}}%）通用读取，未启用专用解析模块',
        })}
      </p>

      {recommendation ? (
        <>
          <p className="missing-module-hint__recommend">
            {t('workbench.files.hint.recommend', {
              fileName: entry.name,
              module: recommendation.displayName,
              defaultValue: '检测到「{{fileName}}」日志特征，建议安装 {{module}}',
            })}
          </p>
          <p className="missing-module-hint__adds-title">
            {t('workbench.files.hint.adds_title', { defaultValue: '安装后新增能力' })}
          </p>
          <ul className="missing-module-hint__adds">
            {recommendation.adds.map((add) => (
              <li key={add}>{add}</li>
            ))}
          </ul>
        </>
      ) : (
        <p className="missing-module-hint__generic">
          {t('workbench.files.hint.generic', {
            defaultValue: '此日志可能缺少专用解析模块，当前仅按通用格式读取',
          })}
        </p>
      )}

      <div className="missing-module-hint__actions">
        <button
          type="button"
          className="missing-module-hint__btn missing-module-hint__btn--primary"
          onClick={() => void onAddModule()}
          disabled={busy}
          data-testid="hint-add-module-btn"
        >
          {busy
            ? t('workbench.files.hint.installing', { defaultValue: '正在安装…' })
            : t('workbench.files.hint.add_module', { defaultValue: '添加模块' })}
        </button>
        <button
          type="button"
          className="missing-module-hint__btn"
          onClick={onDismiss}
          data-testid="hint-dismiss-btn"
        >
          {t('workbench.files.hint.continue_generic', { defaultValue: '继续以通用方式读取' })}
        </button>
      </div>

      {installed && (
        <p className="missing-module-hint__success" data-testid="hint-install-success">
          {t('workbench.files.hint.install_success', {
            defaultValue: '模块已安装，请重新导入该文件以启用完整解析',
          })}
        </p>
      )}
      {error && (
        <p className="missing-module-hint__error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** ui/src/components/modules/ModuleOnboardingDialog.tsx — 模块化首启引导（P1-01 建议 1-2）。
 *  在 main.tsx 的 SessionProvider 内、AppShell 旁挂载；localStorage 'ab.module.onboarded'
 *  缺失时首次显示一次，之后（稍后再说/×）不再打扰；插件页始终保留安装入口。
 *  安装仅面向用户选择的本地 ZIP（pickPluginZip + installPluginZip），无静默联网下载。 */

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { pickPluginZip } from '../../ipc/real';
import { useSession } from '../../state/session';
import { KNOWN_MODULES } from './knownModules';
import './ModuleOnboardingDialog.css';

/** 首启偏好键：存在即视为已完成引导（不再弹出）。 */
export const ONBOARDED_KEY = 'ab.module.onboarded';

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

export default function ModuleOnboardingDialog() {
  const { state, actions } = useSession();
  const { t } = useTranslation();
  const [open, setOpen] = useState(() => localStorage.getItem(ONBOARDED_KEY) === null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [installed, setInstalled] = useState(false);

  const closeAndRemember = () => {
    localStorage.setItem(ONBOARDED_KEY, '1');
    setOpen(false);
  };

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

  if (!open) return null;

  return (
    <div className="module-onboarding" data-testid="module-onboarding" role="dialog" aria-modal="true">
      <div className="module-onboarding__card">
        <button
          type="button"
          className="module-onboarding__close"
          onClick={closeAndRemember}
          aria-label={t('modules.onboarding.close', { defaultValue: '关闭' })}
          data-testid="onboarding-close-btn"
        >
          ×
        </button>

        <h2 className="module-onboarding__title">
          {t('modules.onboarding.title', { defaultValue: '安装分析模块' })}
        </h2>
        <p className="module-onboarding__body">
          {t('modules.onboarding.body', {
            defaultValue: 'AnalysisBuddy 的分析能力由模块提供。您可以从本地 ZIP 添加专用解析模块，以获得完整的传感器、电池等指标与上下文。',
          })}
        </p>

        <section className="module-onboarding__section">
          <h3 className="module-onboarding__heading">
            {t('modules.onboarding.installed_title', { defaultValue: '已安装模块' })}
          </h3>
          {state.plugins.length === 0 ? (
            <p className="module-onboarding__empty">
              {t('modules.onboarding.installed_empty', { defaultValue: '暂无已安装模块' })}
            </p>
          ) : (
            <ul className="module-onboarding__list">
              {state.plugins.map((p) => (
                <li key={p.id} className="module-onboarding__item" data-testid="installed-module">
                  <span className="module-onboarding__item-name">{p.display_name}</span>
                  <span className="module-onboarding__item-id">{p.id}</span>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section className="module-onboarding__section">
          <h3 className="module-onboarding__heading">
            {t('modules.onboarding.recommended_title', { defaultValue: '推荐模块' })}
          </h3>
          <ul className="module-onboarding__list">
            {KNOWN_MODULES.map((m) => (
              <li key={m.moduleId} className="module-onboarding__item" data-testid="recommended-module">
                <span className="module-onboarding__item-name">{m.displayName}</span>
                <span className="module-onboarding__item-src">
                  {t('modules.onboarding.from_zip', { defaultValue: '从本地 ZIP 安装' })}
                </span>
              </li>
            ))}
          </ul>
        </section>

        <div className="module-onboarding__actions">
          <button
            type="button"
            className="module-onboarding__btn module-onboarding__btn--primary"
            onClick={() => void onAddModule()}
            disabled={busy}
            data-testid="onboarding-add-btn"
          >
            {busy
              ? t('modules.onboarding.installing', { defaultValue: '正在安装…' })
              : t('modules.onboarding.add', { defaultValue: '添加模块' })}
          </button>
          <button
            type="button"
            className="module-onboarding__btn"
            onClick={closeAndRemember}
            data-testid="onboarding-later-btn"
          >
            {t('modules.onboarding.later', { defaultValue: '稍后再说' })}
          </button>
        </div>

        {installed && (
          <p className="module-onboarding__success" data-testid="onboarding-install-success">
            {t('modules.onboarding.install_success', { defaultValue: '模块已安装' })}
          </p>
        )}
        {error && (
          <p className="module-onboarding__error" role="alert">
            {error}
          </p>
        )}
      </div>
    </div>
  );
}

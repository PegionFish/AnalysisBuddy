import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ipc, useMockIpc } from '../ipc/ipc';
import { reportError } from '../lib/globalErrors';
import { useSession } from '../state/session';
import './TopBar.css';

interface TopBarProps {
  route: string;
  onNavigate: (route: string) => void;
}

/** App chrome: session ops, language/theme switches, missing-files badge, nav (ipc-ui.md §4.1). */
export default function TopBar({ route, onNavigate }: TopBarProps) {
  const { state, actions, saveError, dismissSaveError, saveNotice, dismissSaveNotice } = useSession();
  const { t } = useTranslation();
  const [openPath, setOpenPath] = useState('');
  const mock = useMockIpc();

  const openSession = () => {
    if (!mock || !openPath.trim()) return;
    void actions.openSession(openPath.trim());
    setOpenPath('');
  };

  // 生产模式打开会话（契约 C3.1）：原生文件选择器 → 原子替换装载。
  // 取消静默；其余失败留痕到全局错误横幅（禁止静默吞错，任务 21）。
  const pickAndOpenSession = () => {
    void ipc
      .pickOpenSession()
      .then((path) => {
        if (path === null) return;
        void actions.openSession(path).catch((e) => reportError(e, 'open_session'));
      })
      .catch((e) => reportError(e, 'pick_open_session'));
  };

  /** P10：新建会话是破坏性操作（丢弃全部文件/指标/游标）——确认后才重置。
   *  项目内无通用确认对话框组件（仅 PluginManagerPage 内联覆盖确认），以 window.confirm
   *  兜底（WebView2 支持原生 confirm）。仅显式「取消」（false）中止：Tauri 恒返回布尔；
   *  jsdom 未实现 confirm 时返回 undefined，按确认处理以保持既有测试语义。 */
  const confirmNewSession = () => {
    const ok = window.confirm(
      t('workbench.topbar.new_session_confirm', {
        defaultValue: 'Start a new session? All unsaved files, selected metrics, and cursor state will be lost.',
      }),
    );
    if (ok === false) return;
    actions.newSession();
  };

  const missingCount = state.missing.length;
  const reopenFailedCount = state.reopenFailed.length;
  const reopenFailedPaths = state.reopenFailed.map((e) => e.path).join('\n');

  return (
    <header className="topbar">
      <nav className="topbar__nav">
        <button
          type="button"
          className={route === '/' ? 'topbar__nav-link topbar__nav-link--active' : 'topbar__nav-link'}
          onClick={() => onNavigate('/')}
        >
          {t('workbench.nav.workbench')}
        </button>
        <button
          type="button"
          className={route === '/plugins' ? 'topbar__nav-link topbar__nav-link--active' : 'topbar__nav-link'}
          onClick={() => onNavigate('/plugins')}
        >
          {t('workbench.nav.plugins')}
        </button>
      </nav>

      {missingCount > 0 && (
        <span className="topbar__missing" role="status" data-testid="missing-badge">
          {t('workbench.topbar.missing_files', { count: missingCount })}
        </span>
      )}

      {reopenFailedCount > 0 && (
        <span
          className="topbar__missing"
          role="status"
          data-testid="reopen-failed-badge"
          title={reopenFailedPaths}
        >
          {t('workbench.topbar.reopen_failed_files', { count: reopenFailedCount })}
        </span>
      )}

      <div className="topbar__spacer" />

      {!mock && (
        <button
          type="button"
          className="topbar__btn"
          onClick={() => void pickAndOpenSession()}
          data-testid="open-session-pick"
        >
          {t('workbench.topbar.open_session_pick', { defaultValue: 'Open Session…' })}
        </button>
      )}

      {mock && (
        <div className="topbar__open">
          <input
            type="text"
            className="topbar__input"
            placeholder={t('workbench.topbar.session_path_hint')}
            value={openPath}
            onChange={(e) => setOpenPath(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') openSession();
            }}
            aria-label={t('workbench.topbar.session_path_hint')}
          />
          <button type="button" className="topbar__btn" onClick={openSession} disabled={!openPath.trim()}>
            {t('workbench.topbar.open_session')}
          </button>
        </div>
      )}

      <button type="button" className="topbar__btn" onClick={() => void actions.saveSession()}>
        {t('workbench.topbar.save_session')}
      </button>
      <button type="button" className="topbar__btn" onClick={() => void actions.saveSessionAs()}>
        {t('workbench.topbar.save_session_as')}
      </button>
      <button type="button" className="topbar__btn" onClick={confirmNewSession}>
        {t('workbench.topbar.new_session')}
      </button>

      <select
        className="topbar__select"
        aria-label={t('workbench.topbar.language')}
        value={state.lang}
        onChange={(e) => actions.setLang(e.target.value as 'zh' | 'en')}
      >
        <option value="zh">中文</option>
        <option value="en">English</option>
      </select>

      <button
        type="button"
        className="topbar__btn"
        aria-label={t('workbench.topbar.theme')}
        onClick={() => actions.setTheme(state.theme === 'dark' ? 'light' : 'dark')}
      >
        {state.theme === 'dark' ? t('workbench.topbar.theme_light') : t('workbench.topbar.theme_dark')}
      </button>

      {/* 保存会话失败横幅（任务 17：此前 save_session 静默无任何反馈） */}
      {saveError && (
        <div className="topbar__save-error" role="alert" data-testid="save-error">
          <span>{saveError}</span>
          <button type="button" className="topbar__save-error-close" onClick={dismissSaveError} aria-label={t('common.error.dismiss')}>
            ×
          </button>
        </div>
      )}

      {/* 保存会话成功 toast（P8：与错误横幅对称，成功色，自动消退） */}
      {saveNotice && (
        <div className="topbar__save-notice" role="status" data-testid="save-notice">
          <span>{saveNotice}</span>
          <button type="button" className="topbar__save-notice-close" onClick={dismissSaveNotice} aria-label={t('common.error.dismiss')}>
            ×
          </button>
        </div>
      )}
    </header>
  );
}

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useMockIpc } from '../ipc/ipc';
import { useSession } from '../state/session';
import './TopBar.css';

interface TopBarProps {
  route: string;
  onNavigate: (route: string) => void;
}

/** App chrome: session ops, language/theme switches, missing-files badge, nav (ipc-ui.md §4.1). */
export default function TopBar({ route, onNavigate }: TopBarProps) {
  const { state, actions, saveError, dismissSaveError } = useSession();
  const { t } = useTranslation();
  const [openPath, setOpenPath] = useState('');
  const mock = useMockIpc();

  const openSession = () => {
    if (!mock || !openPath.trim()) return;
    void actions.openSession(openPath.trim());
    setOpenPath('');
  };

  const missingCount = state.missing.length;

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

      <div className="topbar__spacer" />

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
      <button type="button" className="topbar__btn" onClick={actions.newSession}>
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
    </header>
  );
}

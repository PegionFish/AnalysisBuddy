import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ipc } from '../ipc/ipc';
import { useSession } from '../state/session';
import FilePanel from './FilePanel';
import MetricTree from './MetricTree';
import TopBar from './TopBar';
import './AppShell.css';

function readRoute(): string {
  const hash = window.location.hash.replace(/^#/, '');
  return hash.startsWith('/plugins') ? '/plugins' : '/';
}

/** Three-column app shell with hash routing between the workbench and the plugin manager (ipc-ui.md §4.1). */
export default function AppShell() {
  const { dispatch } = useSession();
  const { t } = useTranslation();
  const [route, setRoute] = useState<string>(readRoute);

  useEffect(() => {
    const onHash = () => setRoute(readRoute());
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []);

  const navigate = (route: string) => {
    window.location.hash = route;
    setRoute(route);
  };

  useEffect(() => {
    void ipc.list_plugins().then((plugins) => dispatch({ type: 'plugins/set', plugins }));
  }, [dispatch]);

  return (
    <div className="app-shell">
      <TopBar route={route} onNavigate={navigate} />
      {route === '/plugins' ? (
        <main className="app-shell__plugins" data-testid="plugins-page">
          <h2>{t('plugins.list.title')}</h2>
          <p>{t('plugins.stderr.title')}</p>
        </main>
      ) : (
        <div className="app-shell__body">
          <aside className="app-shell__left">
            <FilePanel />
            <MetricTree />
          </aside>
          <main className="app-shell__center">
            <div className="panel app-shell__placeholder">
              <h3>{t('workbench.chart.empty_title')}</h3>
              <p>{t('workbench.chart.empty_body')}</p>
            </div>
          </main>
          <aside className="app-shell__right">
            <div className="panel app-shell__placeholder">
              <h3>{t('workbench.keyvalues.title')}</h3>
              <p>{t('workbench.keyvalues.no_cursor')}</p>
            </div>
          </aside>
        </div>
      )}
    </div>
  );
}

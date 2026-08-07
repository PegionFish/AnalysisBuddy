import { useEffect, useState } from 'react';
import { ipc } from '../ipc/ipc';
import { useSession } from '../state/session';
import FilePanel from './FilePanel';
import KeyValuesPanel from './KeyValuesPanel';
import MetricTree from './MetricTree';
import PluginManagerPage from './PluginManagerPage';
import TimelineChart from './TimelineChart';
import TopBar from './TopBar';
import './AppShell.css';

function readRoute(): string {
  const hash = window.location.hash.replace(/^#/, '');
  return hash.startsWith('/plugins') ? '/plugins' : '/';
}

/** Three-column app shell with hash routing between the workbench and the plugin manager (ipc-ui.md §4.1). */
export default function AppShell() {
  const { dispatch } = useSession();
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
        <main className="app-shell__plugins">
          <PluginManagerPage />
        </main>
      ) : (
        <div className="app-shell__body">
          <aside className="app-shell__left">
            <FilePanel />
            <MetricTree />
          </aside>
          <main className="app-shell__center">
            <TimelineChart />
          </main>
          <aside className="app-shell__right">
            <KeyValuesPanel />
          </aside>
        </div>
      )}
    </div>
  );
}

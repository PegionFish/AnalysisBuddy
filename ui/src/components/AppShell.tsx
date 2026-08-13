import { useEffect, useState } from 'react';
import type { CSSProperties, PointerEvent as ReactPointerEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { ipc } from '../ipc/ipc';
import { useSession } from '../state/session';
import FilePanel from './FilePanel';
import KeyValuesPanel from './KeyValuesPanel';
import MetricTree from './MetricTree';
import PluginManagerPage from './PluginManagerPage';
import TimelineChart from './TimelineChart';
import TopBar from './TopBar';
import './AppShell.css';

const LS_LEFT_COLLAPSED = 'ab.layout.leftCollapsed';
const LS_RIGHT_COLLAPSED = 'ab.layout.rightCollapsed';
const LS_LEFT_W = 'ab.layout.leftW';
const LS_RIGHT_W = 'ab.layout.rightW';

const LEFT_W_MIN = 200;
const LEFT_W_MAX = 480;
const RIGHT_W_MIN = 220;
const RIGHT_W_MAX = 420;
const LEFT_W_DEFAULT = 280;
const RIGHT_W_DEFAULT = 320;
/** Collapsed sidebars keep a narrow rail holding the re-expand button (P2-04). */
const RAIL_W = 44;
const NARROW_QUERY = '(max-width: 999px)';

/** localStorage reads with corruption tolerance: bad/absent values fall back to defaults. */
function readBool(key: string, fallback: boolean): boolean {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return fallback;
    return raw === '1' || raw === 'true';
  } catch {
    return fallback;
  }
}

function readWidth(key: string, fallback: number, min: number, max: number): number {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return fallback;
    const n = Number(raw);
    if (!Number.isFinite(n)) return fallback;
    return Math.min(max, Math.max(min, Math.round(n)));
  } catch {
    return fallback;
  }
}

function writeLs(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* storage unavailable (e.g. hardened webview) — state still works for the session */
  }
}

/** matchMedia-based media query hook; degrades to false (wide layout) when matchMedia is missing. */
function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState<boolean>(() => window.matchMedia?.(query)?.matches ?? false);
  useEffect(() => {
    const mql = window.matchMedia?.(query);
    if (!mql) return;
    const onChange = (e: MediaQueryListEvent) => setMatches(e.matches);
    mql.addEventListener('change', onChange);
    return () => mql.removeEventListener('change', onChange);
  }, [query]);
  return matches;
}

function readRoute(): string {
  const hash = window.location.hash.replace(/^#/, '');
  return hash.startsWith('/plugins') ? '/plugins' : '/';
}

/** Three-column app shell with hash routing between the workbench and the plugin manager (ipc-ui.md §4.1).
 *  P2-04: collapsible + drag-resizable sidebars (CSS vars --ab-left-w/--ab-right-w, localStorage-persisted);
 *  below 1000px the key-values panel moves into a floating drawer instead of a fixed third column. */
export default function AppShell() {
  const { dispatch } = useSession();
  const { t } = useTranslation();
  const [route, setRoute] = useState<string>(readRoute);
  const narrow = useMediaQuery(NARROW_QUERY);
  const [drawerOpen, setDrawerOpen] = useState(false);

  const [leftCollapsed, setLeftCollapsed] = useState<boolean>(() =>
    readBool(LS_LEFT_COLLAPSED, false),
  );
  const [rightCollapsed, setRightCollapsed] = useState<boolean>(() =>
    readBool(LS_RIGHT_COLLAPSED, false),
  );
  const [leftW, setLeftW] = useState<number>(() => readWidth(LS_LEFT_W, LEFT_W_DEFAULT, LEFT_W_MIN, LEFT_W_MAX));
  const [rightW, setRightW] = useState<number>(() =>
    readWidth(LS_RIGHT_W, RIGHT_W_DEFAULT, RIGHT_W_MIN, RIGHT_W_MAX),
  );

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

  const toggleLeft = () =>
    setLeftCollapsed((v) => {
      const next = !v;
      writeLs(LS_LEFT_COLLAPSED, next ? '1' : '0');
      return next;
    });

  const toggleRight = () =>
    setRightCollapsed((v) => {
      const next = !v;
      writeLs(LS_RIGHT_COLLAPSED, next ? '1' : '0');
      return next;
    });

  /** Pointer-driven drag: only CSS variables change, no DOM reflow beyond the grid (plan §6). */
  const startResize = (which: 'left' | 'right', e: ReactPointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const min = which === 'left' ? LEFT_W_MIN : RIGHT_W_MIN;
    const max = which === 'left' ? LEFT_W_MAX : RIGHT_W_MAX;
    document.body.classList.add('ab-resizing');
    const onMove = (ev: PointerEvent) => {
      const next = which === 'left' ? ev.clientX : window.innerWidth - ev.clientX;
      const clamped = Math.min(max, Math.max(min, Math.round(next)));
      if (which === 'left') {
        setLeftW(clamped);
        writeLs(LS_LEFT_W, String(clamped));
      } else {
        setRightW(clamped);
        writeLs(LS_RIGHT_W, String(clamped));
      }
    };
    const onUp = () => {
      document.body.classList.remove('ab-resizing');
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  };

  const cssVars = {
    '--ab-left-w': `${leftCollapsed ? RAIL_W : leftW}px`,
    '--ab-right-w': `${rightCollapsed ? RAIL_W : rightW}px`,
  } as CSSProperties;

  const leftRailButton = (
    <button
      type="button"
      className="app-shell__collapse"
      onClick={toggleLeft}
      aria-label={
        leftCollapsed
          ? t('workbench.layout.expand_left', { defaultValue: '展开左栏' })
          : t('workbench.layout.collapse_left', { defaultValue: '收起左栏' })
      }
      aria-expanded={!leftCollapsed}
      data-testid="collapse-left"
    >
      <span className="app-shell__collapse-icon" aria-hidden="true">
        {leftCollapsed ? '›' : '‹'}
      </span>
    </button>
  );

  const rightRailButton = (
    <button
      type="button"
      className="app-shell__collapse"
      onClick={toggleRight}
      aria-label={
        rightCollapsed
          ? t('workbench.layout.expand_right', { defaultValue: '展开右栏' })
          : t('workbench.layout.collapse_right', { defaultValue: '收起右栏' })
      }
      aria-expanded={!rightCollapsed}
      data-testid="collapse-right"
    >
      <span className="app-shell__collapse-icon" aria-hidden="true">
        {rightCollapsed ? '‹' : '›'}
      </span>
    </button>
  );

  return (
    <div className="app-shell">
      <TopBar route={route} onNavigate={navigate} />
      {route === '/plugins' ? (
        <main className="app-shell__plugins">
          <PluginManagerPage />
        </main>
      ) : (
        <>
          <div
            className={`app-shell__body${narrow ? ' app-shell__body--narrow' : ''}`}
            style={cssVars}
          >
            <aside
              className={`app-shell__left${leftCollapsed ? ' app-shell__left--collapsed' : ''}`}
              data-testid="app-shell-left"
            >
              {leftRailButton}
              {!leftCollapsed && (
                <>
                  <div className="app-shell__aside-content app-shell__left-content">
                    <FilePanel />
                    <MetricTree />
                  </div>
                  <div
                    className="app-shell__handle app-shell__handle--left"
                    role="separator"
                    aria-orientation="vertical"
                    aria-label={t('workbench.layout.resize_left', { defaultValue: '调整左栏宽度' })}
                    data-testid="resize-left"
                    onPointerDown={(e) => startResize('left', e)}
                  />
                </>
              )}
            </aside>
            <main className="app-shell__center">
              <TimelineChart />
            </main>
            {!narrow && (
              <aside
                className={`app-shell__right${rightCollapsed ? ' app-shell__right--collapsed' : ''}`}
                data-testid="app-shell-right"
              >
                {rightRailButton}
                {!rightCollapsed && (
                  <>
                    <div className="app-shell__aside-content app-shell__right-content">
                      <KeyValuesPanel />
                    </div>
                    <div
                      className="app-shell__handle app-shell__handle--right"
                      role="separator"
                      aria-orientation="vertical"
                      aria-label={t('workbench.layout.resize_right', { defaultValue: '调整右栏宽度' })}
                      data-testid="resize-right"
                      onPointerDown={(e) => startResize('right', e)}
                    />
                  </>
                )}
              </aside>
            )}
          </div>
          {narrow && (
            <button
              type="button"
              className="app-shell__drawer-toggle"
              aria-expanded={drawerOpen}
              onClick={() => setDrawerOpen((o) => !o)}
              data-testid="kv-drawer-toggle"
            >
              {t('workbench.layout.kv_toggle', { defaultValue: '关键值' })}
            </button>
          )}
          {narrow && drawerOpen && (
            <div className="app-shell__drawer" data-testid="kv-drawer">
              <div className="app-shell__drawer-head">
                <h2 className="app-shell__drawer-title">
                  {t('workbench.layout.kv_toggle', { defaultValue: '关键值' })}
                </h2>
                <button
                  type="button"
                  className="app-shell__drawer-close"
                  aria-label={t('workbench.layout.close_drawer', { defaultValue: '关闭关键值抽屉' })}
                  onClick={() => setDrawerOpen(false)}
                  data-testid="kv-drawer-close"
                >
                  ×
                </button>
              </div>
              <KeyValuesPanel />
            </div>
          )}
        </>
      )}
    </div>
  );
}

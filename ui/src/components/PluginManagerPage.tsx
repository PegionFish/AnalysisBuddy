/** ui/src/components/PluginManagerPage.tsx — /plugins route (ipc-ui.md §4.6).
 *  Plugin list with 10-state health badges (live via ab://plugin-health), stderr log drawer
 *  (get_plugin_log backfill + live append, follow-on-scroll), capabilities, and reload. */

import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { PluginLogPayload } from '../ipc/events';
import { ipc } from '../ipc/ipc';
import { formatTime } from '../lib/format';
import { useSession } from '../state/session';
import './PluginManagerPage.css';

/** Merged view of the log drawer: get_plugin_log backfill ∪ live events (deduped, §2.2 补发约定). */
function mergeLogs(backfill: PluginLogPayload[], live: PluginLogPayload[]): PluginLogPayload[] {
  const key = (l: PluginLogPayload) => `${l.ts_ms}:${l.line}`;
  const seen = new Set(backfill.map(key));
  return [...backfill, ...live.filter((l) => !seen.has(key(l)))];
}

/** Plugin management page: health badges, stderr drawer, reload (§4.6). */
export default function PluginManagerPage() {
  const { state, actions, logs, dispatch } = useSession();
  const { t } = useTranslation();
  const [openId, setOpenId] = useState<string | null>(null);
  const [backfill, setBackfill] = useState<PluginLogPayload[]>([]);
  const [following, setFollowing] = useState(true);
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  const prevScrollRef = useRef(0);

  const openDrawer = (pluginId: string) => {
    if (openId === pluginId) {
      setOpenId(null);
      return;
    }
    setOpenId(pluginId);
    setBackfill([]);
    setFollowing(true);
    prevScrollRef.current = 0;
  };

  // 补发历史：抽屉打开时先 get_plugin_log 拉环形缓冲尾部，再以实时事件增量追加（§2.2）。
  // 同时刷新插件快照（list_plugins）以同步 loaded_file_ids 等后端侧变化（§4.6）。
  useEffect(() => {
    if (!openId) return;
    let cancelled = false;
    void ipc.get_plugin_log({ plugin_id: openId }).then((logs) => {
      if (!cancelled) setBackfill(logs);
    });
    void ipc.list_plugins().then((plugins) => {
      if (!cancelled) dispatch({ type: 'plugins/set', plugins });
    });
    return () => {
      cancelled = true;
    };
  }, [openId, dispatch]);

  const lines = useMemo(
    () => mergeLogs(backfill, openId ? (logs[openId] ?? []) : []),
    [backfill, openId, logs],
  );

  // 自动滚底；用户上滚（scrollTop 回退）时暂停跟随，点「跟随滚动」恢复（§4.6）。
  useEffect(() => {
    const el = scrollerRef.current;
    if (el && following) el.scrollTop = el.scrollHeight;
  }, [lines, following]);

  const onScroll = () => {
    const el = scrollerRef.current;
    if (!el) return;
    if (el.scrollTop < prevScrollRef.current) setFollowing(false);
    prevScrollRef.current = el.scrollTop;
  };

  const openPlugin = state.plugins.find((p) => p.id === openId);

  return (
    <div className="plugin-page" data-testid="plugins-page">
      <h2 className="plugin-page__title">{t('plugins.list.title')}</h2>

      {state.plugins.length === 0 ? (
        <p className="plugin-page__empty">{t('plugins.list.empty')}</p>
      ) : (
        <ul className="plugin-page__list">
          {state.plugins.map((p) => (
            <li key={p.id} className="plugin-row" data-testid="plugin-row" data-plugin-id={p.id}>
              <div className="plugin-row__head">
                <span className="plugin-row__id">{p.id}</span>
                <span className="plugin-row__name">{p.display_name}</span>
                <span
                  className={`plugin-badge plugin-badge--${p.state}`}
                  data-testid="plugin-badge"
                  data-state={p.state}
                >
                  {t(`plugins.list.health.${p.state}`)}
                </span>
              </div>

              <div className="plugin-row__meta">
                <span className="plugin-row__version">v{p.version}</span>
                {p.last_error && (
                  <span className="plugin-row__error" data-testid="plugin-last-error">
                    {t('plugins.list.last_error')}: {p.last_error}
                  </span>
                )}
              </div>

              <div className="plugin-row__toolbar">
                <button
                  type="button"
                  className="plugin-row__btn"
                  onClick={() => void actions.reloadPlugin(p.id)}
                  data-testid="reload-btn"
                >
                  {t('plugins.list.reload')}
                </button>
                <button
                  type="button"
                  className="plugin-row__btn"
                  onClick={() => openDrawer(p.id)}
                  data-testid="drawer-toggle"
                >
                  {t('plugins.list.show_logs')}
                </button>
              </div>

              {openId === p.id && openPlugin && (
                <div className="plugin-drawer" data-testid="plugin-drawer">
                  <div className="plugin-drawer__section">
                    <span className="plugin-drawer__label">{t('plugins.list.capabilities')}</span>
                    <ul className="plugin-drawer__caps">
                      <li>
                        {t('plugins.list.cap_annotate')}:{' '}
                        {p.capabilities.annotate ? t('plugins.list.cap_on') : t('plugins.list.cap_off')}
                      </li>
                      <li>
                        {t('plugins.list.cap_subscribe')}:{' '}
                        {p.capabilities.subscribe ? t('plugins.list.cap_on') : t('plugins.list.cap_off')}
                      </li>
                      <li>
                        {t('plugins.list.cap_sidecar')}:{' '}
                        {p.capabilities.binary_sidecar ? t('plugins.list.cap_on') : t('plugins.list.cap_off')}
                      </li>
                    </ul>
                  </div>

                  <div className="plugin-drawer__section">
                    <span className="plugin-drawer__label">{t('plugins.list.loaded_files')}</span>
                    <div className="plugin-drawer__files" data-testid="loaded-files">
                      {openPlugin.loaded_file_ids.length === 0
                        ? '—'
                        : openPlugin.loaded_file_ids.map((id) => <span key={id}>{id}</span>)}
                    </div>
                  </div>

                  <div className="plugin-drawer__section">
                    <div className="plugin-drawer__log-head">
                      <span className="plugin-drawer__label">{t('plugins.stderr.title')}</span>
                      <button
                        type="button"
                        className="plugin-drawer__follow"
                        onClick={() => setFollowing(true)}
                        data-testid="follow-btn"
                      >
                        {t('plugins.stderr.follow')}
                        {!following && (
                          <span className="plugin-drawer__paused">（{t('plugins.stderr.paused')}）</span>
                        )}
                      </button>
                    </div>
                    <div
                      className="plugin-log__scroller"
                      ref={scrollerRef}
                      onScroll={onScroll}
                      data-testid="log-scroller"
                      data-following={following}
                    >
                      {lines.length === 0 ? (
                        <p className="plugin-log__empty">{t('plugins.list.no_logs')}</p>
                      ) : (
                        lines.map((l, i) => (
                          <div
                            key={`${l.ts_ms}:${l.line}:${i}`}
                            className={`plugin-log__line plugin-log__line--${l.level}`}
                          >
                            <span className="plugin-log__time">{formatTime(l.ts_ms)}</span>
                            <span className="plugin-log__level">{l.level}</span>
                            <span className="plugin-log__text">{l.line}</span>
                          </div>
                        ))
                      )}
                    </div>
                  </div>
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

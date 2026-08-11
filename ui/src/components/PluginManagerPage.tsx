/** ui/src/components/PluginManagerPage.tsx — /plugins route (ipc-ui.md §4.6 + spec §6).
 *  Plugin list with 10-state health badges (live via ab://plugin-health), stderr log drawer
 *  (get_plugin_log backfill + live append, follow-on-scroll), capabilities, reload, and the
 *  module manager: ZIP install dropzone (spec §6.1), 关于/版本历史 details, disable/uninstall,
 *  and the GitHub update flow (check → confirm → update). */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { EV_OS_DRAG_DROP, EV_OS_DRAG_ENTER, EV_OS_DRAG_LEAVE, pickPluginZip } from '../ipc/real';
import type { OsDragDropPayload } from '../ipc/real';
import type { ChangelogEntry, PluginInfo } from '../ipc/types';
import { ipc, useMockIpc } from '../ipc/ipc';
import type { PluginLogPayload } from '../ipc/events';
import { formatTime } from '../lib/format';
import { compareSemver } from '../lib/semver';
import { useSession } from '../state/session';
import './PluginManagerPage.css';

/** 折叠阈值：>20 条 changelog 先展示前 20 条，懒展开（spec §6.2）。 */
const CHANGELOG_COLLAPSE_AT = 20;

/** Merged view of the log drawer: get_plugin_log backfill ∪ live events (deduped, §2.2 补发约定). */
function mergeLogs(backfill: PluginLogPayload[], live: PluginLogPayload[]): PluginLogPayload[] {
  const key = (l: PluginLogPayload) => `${l.ts_ms}:${l.line}`;
  const seen = new Set(backfill.map(key));
  return [...backfill, ...live.filter((l) => !seen.has(key(l)))];
}

/** 拒绝值 → { code, message }（与 FilePanel 同策略：ACL/原生拒绝可能是纯字符串）。 */
function errorText(e: unknown): { code: string; message: string } {
  if (typeof e === 'string') return { code: '', message: e };
  if (e && typeof e === 'object') {
    const obj = e as { code?: unknown; message?: unknown };
    return { code: typeof obj.code === 'string' ? obj.code : '', message: typeof obj.message === 'string' ? obj.message : '' };
  }
  return { code: '', message: '' };
}

/** 版本历史渲染（spec §6.2）：semver 降序、当前版本徽标、notes 列表、空 notes → —、
 *  >20 条折叠「显示全部」；零 markdown 依赖。 */
export function ChangelogSection({
  entries,
  currentVersion,
}: {
  entries: ChangelogEntry[];
  currentVersion: string;
}) {
  const { t } = useTranslation();
  const [showAll, setShowAll] = useState(false);

  // 展示层强制 semver 降序，不信任 manifest 数组顺序（spec §6.2）。
  const sorted = useMemo(
    () => [...entries].sort((a, b) => compareSemver(b.version, a.version)),
    [entries],
  );
  const visible = showAll || sorted.length <= CHANGELOG_COLLAPSE_AT ? sorted : sorted.slice(0, CHANGELOG_COLLAPSE_AT);

  if (sorted.length === 0) {
    return <p className="changelog__empty">{t('plugins.changelog.empty')}</p>;
  }

  return (
    <div className="changelog" data-testid="changelog">
      <ul className="changelog__list">
        {visible.map((entry) => {
          const isCurrent = compareSemver(entry.version, currentVersion) === 0;
          return (
            <li key={entry.version} className="changelog-entry" data-testid="changelog-entry">
              <div className="changelog-entry__head">
                <span className="changelog-entry__version">v{entry.version}</span>
                {isCurrent && (
                  <span className="changelog-entry__current" data-testid="changelog-current">
                    {t('plugins.changelog.current')}
                  </span>
                )}
                <span className="changelog-entry__date">{entry.date}</span>
              </div>
              {entry.notes.length === 0 ? (
                <p className="changelog-entry__no-notes" data-testid="changelog-no-notes">
                  {t('plugins.changelog.no_notes')}
                </p>
              ) : (
                <ul className="changelog-entry__notes">
                  {entry.notes.map((note, i) => (
                    <li key={`${entry.version}:${i}`}>{note}</li>
                  ))}
                </ul>
              )}
            </li>
          );
        })}
      </ul>
      {sorted.length > CHANGELOG_COLLAPSE_AT && !showAll && (
        <button
          type="button"
          className="changelog__show-all"
          onClick={() => setShowAll(true)}
          data-testid="changelog-show-all"
        >
          {t('plugins.changelog.show_all')}
        </button>
      )}
    </div>
  );
}

/** Plugin management page: health badges, stderr drawer, reload, module manager (§4.6 + §6). */
export default function PluginManagerPage() {
  const { state, actions, logs, dispatch } = useSession();
  const { t } = useTranslation();
  const mock = useMockIpc();
  const [openId, setOpenId] = useState<string | null>(null);
  const [backfill, setBackfill] = useState<PluginLogPayload[]>([]);
  const [following, setFollowing] = useState(true);
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  const prevScrollRef = useRef(0);

  // ---- module manager transient UI state (spec §6.1) ----
  const [dragging, setDragging] = useState(false);
  const [pageError, setPageError] = useState<string | null>(null);
  /** 行级操作中（安装/卸载/禁用/更新）→ spinner + 禁用该行按钮（spec §5.2 并发纪律）。 */
  const [busyId, setBusyId] = useState<string | null>(null);
  /** 同 id 不同版本安装 → 覆盖确认（module_conflict，spec §4.2 第⑤步）。 */
  const [confirmOverwrite, setConfirmOverwrite] = useState<{ path: string; version: string | null } | null>(null);
  /** check_plugin_update 结果：found=待确认更新；uptodate=提示已最新（spec §4.3）。 */
  const [updateNotice, setUpdateNotice] = useState<
    { pluginId: string; kind: 'found'; version: string } | { pluginId: string; kind: 'uptodate' } | null
  >(null);

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

  const runInstall = useCallback(
    async (path: string, overwrite: boolean) => {
      setPageError(null);
      try {
        await actions.installPluginZip(path, overwrite);
        setConfirmOverwrite(null);
      } catch (e) {
        const { code, message } = errorText(e);
        if (code === 'module_conflict') {
          const version =
            e && typeof e === 'object' && 'data' in e
              ? ((e as { data?: { version?: string } }).data?.version ?? null)
              : null;
          setConfirmOverwrite({ path, version });
        } else {
          setPageError(
            t('plugins.install.failed', {
              message: t(`common.error.${code}`, { defaultValue: message || t('common.error.internal') }),
            }),
          );
        }
      }
    },
    [actions, t],
  );

  const onPickZip = async () => {
    try {
      const picked = await pickPluginZip();
      if (picked) await runInstall(picked, false);
    } catch (e) {
      const { code, message } = errorText(e);
      setPageError(t('plugins.install.failed', { message: message || t(`common.error.${code}`) }));
    }
  };

  const withBusy = async (pluginId: string, failKey: string | null, fn: () => Promise<void>) => {
    setBusyId(pluginId);
    setPageError(null);
    try {
      await fn();
    } catch (e) {
      const { code, message } = errorText(e);
      const reason = t(`common.error.${code}`, { defaultValue: message || t('common.error.internal') });
      setPageError(failKey ? t(failKey, { message: reason }) : reason);
    } finally {
      setBusyId(null);
    }
  };

  const runUninstall = (pluginId: string) =>
    withBusy(pluginId, 'plugins.uninstall_failed', async () => {
      await actions.uninstallPlugin(pluginId);
    });

  // enabled 参数 = 目标启用态：当前禁用 → 启用（true）；当前启用 → 禁用（false）。
  const runToggleEnabled = (plugin: PluginInfo) =>
    withBusy(plugin.id, null, async () => {
      await actions.setPluginEnabled(plugin.id, plugin.disabled);
    });

  const runCheckUpdate = (pluginId: string) =>
    withBusy(pluginId, 'plugins.update.failed', async () => {
      const info = await ipc.check_plugin_update({ plugin_id: pluginId });
      setUpdateNotice(
        info.is_newer && info.latest_version
          ? { pluginId, kind: 'found', version: info.latest_version }
          : { pluginId, kind: 'uptodate' },
      );
    });

  const runUpdate = (pluginId: string) =>
    withBusy(pluginId, 'plugins.update.failed', async () => {
      await actions.updatePlugin(pluginId);
      setUpdateNotice(null);
    });

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

  // OS 拖放（real 模式主入口）：Tauri 2 拦截 OS 拖放，HTML5 drop 拿不到真实路径
  // ——订阅 core 拖放事件，过滤 .zip（spec §6.1；与 FilePanel 同模式）。
  useEffect(() => {
    if (mock) return;
    const unDrop = ipc.listen<OsDragDropPayload>(EV_OS_DRAG_DROP, (payload) => {
      setDragging(false);
      const zip = payload.paths.find((p) => p.toLowerCase().endsWith('.zip'));
      if (zip) void runInstall(zip, false);
    });
    const unEnter = ipc.listen(EV_OS_DRAG_ENTER, () => setDragging(true));
    const unLeave = ipc.listen(EV_OS_DRAG_LEAVE, () => setDragging(false));
    return () => {
      unDrop();
      unEnter();
      unLeave();
    };
  }, [mock, runInstall]);

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

      <section className="plugin-page__install">
        <div
          className={`plugin-page__dropzone${dragging ? ' plugin-page__dropzone--over' : ''}`}
          data-testid="plugin-dropzone"
          onDragOver={(e) => {
            e.preventDefault();
            setDragging(true);
          }}
          onDragLeave={() => setDragging(false)}
          onDrop={(e) => {
            e.preventDefault();
            setDragging(false);
            // Mock 模式回退：HTML5 drop 只有文件名（无路径）；真实路径走 tauri://drag-drop。
            if (mock) {
              const zip = [...e.dataTransfer.files].map((f) => f.name).find((n) => n.toLowerCase().endsWith('.zip'));
              if (zip) void runInstall(zip, false);
            }
          }}
        >
          <span>{t('plugins.install.drop_hint')}</span>
          {!mock && (
            <button
              type="button"
              className="plugin-page__pick"
              onClick={() => void onPickZip()}
              data-testid="pick-plugin-zip-btn"
            >
              {t('plugins.install.pick_files')}
            </button>
          )}
        </div>

        {confirmOverwrite && (
          <div className="plugin-page__conflict" role="alert" data-testid="install-conflict">
            <span>
              {confirmOverwrite.version
                ? t('plugins.install.conflict_body', { version: confirmOverwrite.version })
                : t('plugins.install.conflict_generic')}
            </span>
            <button
              type="button"
              className="plugin-page__btn"
              onClick={() => void runInstall(confirmOverwrite.path, true)}
              data-testid="install-overwrite-btn"
            >
              {t('plugins.install.overwrite')}
            </button>
            <button type="button" className="plugin-page__btn" onClick={() => setConfirmOverwrite(null)}>
              {t('plugins.install.cancel')}
            </button>
          </div>
        )}

        {pageError && (
          <div className="plugin-page__error" role="alert" data-testid="plugin-page-error">
            {pageError}
          </div>
        )}
      </section>

      {state.plugins.length === 0 ? (
        <p className="plugin-page__empty">{t('plugins.list.empty')}</p>
      ) : (
        <ul className="plugin-page__list">
          {state.plugins.map((p) => (
            <li
              key={p.id}
              className={`plugin-row${p.disabled ? ' plugin-row--disabled' : ''}`}
              data-testid="plugin-row"
              data-plugin-id={p.id}
            >
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
                {p.builtin && (
                  <span className="plugin-row__marker" data-testid="builtin-marker">
                    {t('plugins.builtin')}
                  </span>
                )}
                {p.disabled && (
                  <span className="plugin-row__marker plugin-row__marker--disabled" data-testid="disabled-marker">
                    {t('plugins.disabled')}
                  </span>
                )}
                <span className="plugin-row__source">{t(`plugins.source.${p.source}`)}</span>
                {p.last_error && (
                  <span className="plugin-row__error" data-testid="plugin-last-error">
                    {t('plugins.list.last_error')}: {p.last_error}
                  </span>
                )}
              </div>

              <div className="plugin-row__toolbar">
                {busyId === p.id && <span className="plugin-row__spinner" data-testid="row-spinner" aria-label="busy" />}
                <button
                  type="button"
                  className="plugin-row__btn"
                  onClick={() => void runToggleEnabled(p)}
                  disabled={busyId === p.id}
                  data-testid="toggle-enabled-btn"
                >
                  {p.disabled ? t('plugins.enable') : t('plugins.disable')}
                </button>
                {!p.builtin && (
                  <button
                    type="button"
                    className="plugin-row__btn"
                    onClick={() => void runUninstall(p.id)}
                    disabled={busyId === p.id}
                    data-testid="uninstall-plugin-btn"
                  >
                    {t('plugins.uninstall')}
                  </button>
                )}
                {p.update_url && (
                  <button
                    type="button"
                    className="plugin-row__btn"
                    onClick={() => void runCheckUpdate(p.id)}
                    disabled={busyId === p.id}
                    data-testid="check-update-btn"
                  >
                    {t('plugins.update.check')}
                  </button>
                )}
                <button
                  type="button"
                  className="plugin-row__btn"
                  onClick={() => void actions.reloadPlugin(p.id)}
                  disabled={busyId === p.id}
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

              {updateNotice?.pluginId === p.id && updateNotice.kind === 'found' && (
                <div className="plugin-row__update-confirm" data-testid="update-confirm">
                  <span>{t('plugins.update.found', { version: updateNotice.version })}</span>
                  <button
                    type="button"
                    className="plugin-row__btn"
                    onClick={() => void runUpdate(p.id)}
                    disabled={busyId === p.id}
                    data-testid="update-confirm-btn"
                  >
                    {t('plugins.update.confirm')}
                  </button>
                  <button
                    type="button"
                    className="plugin-row__btn"
                    onClick={() => setUpdateNotice(null)}
                    disabled={busyId === p.id}
                  >
                    {t('plugins.update.cancel')}
                  </button>
                </div>
              )}
              {updateNotice?.pluginId === p.id && updateNotice.kind === 'uptodate' && (
                <p className="plugin-row__update-notice" data-testid="update-up-to-date">
                  {t('plugins.update.up_to_date')}
                </p>
              )}

              {openId === p.id && openPlugin && (
                <div className="plugin-drawer" data-testid="plugin-drawer">
                  <div className="plugin-drawer__section">
                    <span className="plugin-drawer__label">{t('plugins.about.title')}</span>
                    <ul className="plugin-drawer__about" data-testid="plugin-about">
                      {openPlugin.author ? (
                        <li>
                          {t('plugins.about.author')}: {openPlugin.author}
                        </li>
                      ) : null}
                      {openPlugin.repository ? (
                        <li>
                          {t('plugins.about.repository')}:{' '}
                          <a href={openPlugin.repository} target="_blank" rel="noreferrer">
                            {openPlugin.repository}
                          </a>
                        </li>
                      ) : null}
                      {openPlugin.tools && openPlugin.tools.length > 0 ? (
                        <li>
                          {t('plugins.about.tools')}: {openPlugin.tools.join('; ')}
                        </li>
                      ) : null}
                      {!openPlugin.author &&
                        !openPlugin.repository &&
                        (!openPlugin.tools || openPlugin.tools.length === 0) && (
                          <li className="plugin-drawer__about-empty">{t('plugins.about.empty')}</li>
                        )}
                    </ul>
                  </div>

                  <div className="plugin-drawer__section">
                    <span className="plugin-drawer__label">{t('plugins.changelog.title')}</span>
                    <ChangelogSection entries={openPlugin.changelog ?? []} currentVersion={openPlugin.version} />
                  </div>

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

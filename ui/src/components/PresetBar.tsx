import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ipc } from '../ipc/ipc';
import type { LocalizedName, PresetDef, UserPreset } from '../ipc/types';
import { reportError } from '../lib/globalErrors';
import { matchPreset, matchUserPreset, type PresetMatchResult } from '../lib/presetMatch';
import { useSession } from '../state/session';
import './PresetBar.css';

/** 本地 toast 自动消退时长（与会话层 SAVE_NOTICE_TTL_MS 对齐）。 */
const NOTICE_TTL_MS = 4000;

interface PluginPresetOption {
  key: string;
  kind: 'plugin';
  pluginId: string;
  pluginName: string;
  preset: PresetDef;
  name: LocalizedName;
}

interface UserPresetOption {
  key: string;
  kind: 'user';
  preset: UserPreset;
  name: LocalizedName;
}

type PresetOption = PluginPresetOption | UserPresetOption;

/** 场景预设工具条（Wave 4 C10）：合并插件预设 + 用户预设，应用/保存/删除。
 *  全部交互态为组件本地 state（不进 SessionContext）；插件预设随
 *  state.plugins 变化重取（会话层已订阅 EV_PLUGINS_RELOADED → plugins/set），
 *  用户预设列表在挂载/保存成功/删除成功后重取。 */
export default function PresetBar() {
  const { state, actions } = useSession();
  const { t } = useTranslation();

  const [userPresets, setUserPresets] = useState<UserPreset[]>([]);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [saveOpen, setSaveOpen] = useState(false);
  const [saveName, setSaveName] = useState('');
  const [saveNameError, setSaveNameError] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [notice, setNotice] = useState<string[] | null>(null);
  const noticeTimerRef = useRef<number | null>(null);

  /** 重取用户预设列表（挂载/保存成功/删除成功后调用）。 */
  const refreshUserPresets = useCallback(() => {
    void ipc
      .list_user_presets()
      .then((list) => setUserPresets(list))
      // 任务 21：禁止静默吞错——留痕到 console + 全局错误横幅/持久日志。
      .catch((e) => reportError(e, 'list_user_presets'));
  }, []);

  useEffect(() => {
    refreshUserPresets();
  }, [refreshUserPresets]);

  /** 卸载时清掉未触发的 toast 自动消退定时器。 */
  useEffect(() => () => {
    if (noticeTimerRef.current !== null) window.clearTimeout(noticeTimerRef.current);
  }, []);

  /** 插件预设：随 state.plugins（会话层维护，含 EV_PLUGINS_RELOADED 重取）派生。 */
  const pluginOptions = useMemo<PluginPresetOption[]>(
    () =>
      state.plugins.flatMap((p) =>
        (p.presets ?? []).map((preset) => ({
          key: `plugin:${p.id}:${preset.id}`,
          kind: 'plugin' as const,
          pluginId: p.id,
          pluginName: p.display_name,
          preset,
          name: preset.name,
        })),
      ),
    [state.plugins],
  );

  const userOptions = useMemo<UserPresetOption[]>(
    () =>
      userPresets.map((preset) => ({
        key: `user:${preset.id}`,
        kind: 'user' as const,
        preset,
        name: preset.name,
      })),
    [userPresets],
  );

  const options = useMemo<PresetOption[]>(() => [...pluginOptions, ...userOptions], [pluginOptions, userOptions]);

  const selected = useMemo(() => options.find((o) => o.key === selectedKey) ?? null, [options, selectedKey]);

  /** 选中项随清单变化失效（如删除/插件移除）：清选中与确认态。 */
  useEffect(() => {
    if (selectedKey !== null && !options.some((o) => o.key === selectedKey)) {
      setSelectedKey(null);
      setConfirmDelete(false);
    }
  }, [options, selectedKey]);

  const showNotice = useCallback((lines: string[]) => {
    setNotice(lines);
    if (noticeTimerRef.current !== null) window.clearTimeout(noticeTimerRef.current);
    noticeTimerRef.current = window.setTimeout(() => {
      noticeTimerRef.current = null;
      setNotice(null);
    }, NOTICE_TTL_MS);
  }, []);

  const dismissNotice = useCallback(() => {
    if (noticeTimerRef.current !== null) window.clearTimeout(noticeTimerRef.current);
    noticeTimerRef.current = null;
    setNotice(null);
  }, []);

  /** 来源标签：插件预设 = source_plugin + 插件 display_name；用户预设 = source_user。 */
  const sourceLabel = (o: PresetOption): string => {
    if (o.kind === 'plugin') {
      return `${t('presets.bar.source_plugin', { defaultValue: '插件' })} · ${o.pluginName}`;
    }
    return t('presets.bar.source_user', { defaultValue: '我的' });
  };

  const apply = () => {
    if (!selected) return;
    let result: PresetMatchResult;
    if (selected.kind === 'plugin') {
      result = matchPreset(selected.preset, selected.pluginId, state.metricTree);
    } else {
      result = matchUserPreset(selected.preset, state.metricTree);
    }
    const name = selected.name[state.lang];
    const lines: string[] = [];
    if (result.selected.length > 0) {
      // 命中才应用：单次 dispatch 原子替换；零命中绝不 dispatch（组件层保证）。
      actions.applyPreset(result.selected);
      lines.push(t('presets.toast.applied', { name, count: result.selected.length }));
    } else {
      lines.push(t('presets.toast.no_match', { name }));
    }
    if (result.unmatched.length > 0) {
      lines.push(t('presets.toast.unmatched', { count: result.unmatched.length }));
    }
    showNotice(lines);
  };

  const confirmSave = () => {
    const trimmed = saveName.trim();
    if (!trimmed) {
      setSaveNameError(true);
      return;
    }
    setSaveNameError(false);
    setSaveOpen(false);
    setSaveName('');
    // 成功 toast（presets.toast.saved）/失败横幅（presets.toast.save_failed）由会话层
    // 统一呈现（savePresetAs 内部 showNotice/setSaveError）；列表重取在此统一完成。
    void actions.savePresetAs(trimmed).then(() => refreshUserPresets());
  };

  const runDelete = () => {
    if (!selected || selected.kind !== 'user') return;
    const id = selected.preset.id;
    setConfirmDelete(false);
    void ipc
      .delete_user_preset({ id })
      .then(() => {
        showNotice([t('presets.toast.deleted', { defaultValue: 'Preset deleted' })]);
        refreshUserPresets();
      })
      // 任务 21：禁止静默吞错——留痕到 console + 全局错误横幅/持久日志。
      .catch((e) => reportError(e, 'delete_user_preset'));
  };

  const empty = options.length === 0;
  const selectedName = selected ? selected.name[state.lang] : '';

  return (
    <section className="preset-bar" aria-label={t('presets.bar.title', { defaultValue: '预设' })} data-testid="preset-bar">
      <div className="preset-bar__row">
        <select
          className="preset-bar__select"
          aria-label={t('presets.bar.title', { defaultValue: '预设' })}
          data-testid="preset-select"
          value={selectedKey ?? ''}
          disabled={empty}
          onChange={(e) => {
            setSelectedKey(e.target.value || null);
            setConfirmDelete(false);
          }}
        >
          <option value="">{empty ? t('presets.bar.empty', { defaultValue: '暂无预设' }) : ''}</option>
          {options.map((o) => (
            <option key={o.key} value={o.key}>
              {sourceLabel(o)} · {o.name[state.lang]}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="preset-bar__btn preset-bar__btn--primary"
          onClick={apply}
          disabled={!selected}
        >
          {t('presets.bar.apply', { defaultValue: '应用' })}
        </button>
        <button type="button" className="preset-bar__btn" onClick={() => setSaveOpen(true)}>
          {t('presets.bar.save', { defaultValue: '保存为预设…' })}
        </button>
        {selected?.kind === 'user' &&
          (confirmDelete ? (
            <span className="preset-bar__confirm" data-testid="preset-delete-confirm">
              <span className="preset-bar__confirm-text">
                {t('presets.confirm_delete', { name: selectedName, defaultValue: '确定删除预设「{{name}}」？' })}
              </span>
              <button
                type="button"
                className="preset-bar__btn preset-bar__btn--danger"
                onClick={runDelete}
                data-testid="preset-delete-confirm-btn"
              >
                {t('presets.bar.delete', { defaultValue: '删除' })}
              </button>
              <button
                type="button"
                className="preset-bar__btn"
                onClick={() => setConfirmDelete(false)}
                data-testid="preset-delete-cancel-btn"
              >
                {t('presets.save_dialog.cancel', { defaultValue: '取消' })}
              </button>
            </span>
          ) : (
            <button
              type="button"
              className="preset-bar__btn preset-bar__btn--danger"
              onClick={() => setConfirmDelete(true)}
              data-testid="preset-delete"
            >
              {t('presets.bar.delete', { defaultValue: '删除' })}
            </button>
          ))}
      </div>

      {notice && (
        <div className="preset-bar__notice" role="status" data-testid="preset-notice">
          <span className="preset-bar__notice-lines">
            {notice.map((line) => (
              <span key={line} className="preset-bar__notice-line">
                {line}
              </span>
            ))}
          </span>
          <button
            type="button"
            className="preset-bar__notice-close"
            aria-label={t('common.error.dismiss', { defaultValue: '关闭' })}
            onClick={dismissNotice}
          >
            ×
          </button>
        </div>
      )}

      {saveOpen && (
        <div className="preset-bar__dialog" role="dialog" aria-label={t('presets.save_dialog.title', { defaultValue: '保存为预设' })} data-testid="preset-save-dialog">
          <h3 className="preset-bar__dialog-title">{t('presets.save_dialog.title', { defaultValue: '保存为预设' })}</h3>
          <label className="preset-bar__dialog-label" htmlFor="preset-save-name">
            {t('presets.save_dialog.name_label', { defaultValue: '预设名称' })}
          </label>
          <input
            id="preset-save-name"
            type="text"
            className="preset-bar__dialog-input"
            placeholder={t('presets.save_dialog.name_placeholder', { defaultValue: '例如：帧率监控' })}
            value={saveName}
            onChange={(e) => {
              setSaveName(e.target.value);
              setSaveNameError(false);
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter') confirmSave();
            }}
          />
          {saveNameError && (
            <p className="preset-bar__dialog-error" role="alert">
              {t('presets.save_dialog.name_required', { defaultValue: '名称不能为空' })}
            </p>
          )}
          <div className="preset-bar__dialog-actions">
            <button type="button" className="preset-bar__btn preset-bar__btn--primary" onClick={confirmSave}>
              {t('presets.save_dialog.confirm', { defaultValue: '保存' })}
            </button>
            <button
              type="button"
              className="preset-bar__btn"
              onClick={() => {
                setSaveOpen(false);
                setSaveNameError(false);
              }}
            >
              {t('presets.save_dialog.cancel', { defaultValue: '取消' })}
            </button>
          </div>
        </div>
      )}
    </section>
  );
}

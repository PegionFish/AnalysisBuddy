import { useMemo, useState } from 'react';
import type { MetricNode } from '../ipc/types';
import { useSession } from '../state/session';
import { useTranslation } from 'react-i18next';
import PresetBar from './PresetBar';
import './MetricTree.css';

/** P2-01：收藏/最近使用的 localStorage 键；损坏容错（读取失败回落空集/空列表）。 */
const FAVORITES_KEY = 'ab.metric.favorites';
const RECENT_KEY = 'ab.metric.recent';
const RECENT_LIMIT = 10;

/** 语义分组关键词（规范顺序）；指标名命中第一个关键词即归组，无命中落「其他」。 */
const GROUP_KEYWORDS = ['cpu', 'gpu', 'mem', 'disk', 'battery', 'charge', 'power', 'temp'] as const;

/** All metric ids beneath a node, at any depth (a leaf metric node includes itself).
 *  Level-agnostic so both file and plugin rows aggregate exactly their own descendants. */
function collectMetricIds(node: MetricNode): string[] {
  if (node.level === 'metric') return [node.id];
  const ids: string[] = [];
  for (const child of node.children ?? []) ids.push(...collectMetricIds(child));
  return ids;
}

function fileDisplayName(fileId: string, treeName: string, files: { file_id: string; name: string }[]): string {
  return files.find((f) => f.file_id === fileId)?.name ?? treeName;
}

function readStoredIds(key: string): Set<string> {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return new Set();
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return new Set();
    return new Set(parsed.filter((x): x is string => typeof x === 'string'));
  } catch {
    return new Set();
  }
}

function readStoredRecent(key: string): string[] {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((x): x is string => typeof x === 'string').slice(0, RECENT_LIMIT);
  } catch {
    return [];
  }
}

function writeStoredIds(key: string, ids: string[]): void {
  try {
    localStorage.setItem(key, JSON.stringify(ids));
  } catch {
    // 存储不可用（隐私模式等）：保留内存态即可，不影响本次会话。
  }
}

function groupKeyOf(name: string): string {
  const lower = name.toLowerCase();
  for (const keyword of GROUP_KEYWORDS) {
    if (lower.includes(keyword)) return keyword;
  }
  return 'other';
}

function partitionByGroup(metrics: MetricNode[]): { key: string; metrics: MetricNode[] }[] {
  const groups = new Map<string, MetricNode[]>();
  for (const m of metrics) {
    const key = groupKeyOf(m.name);
    const list = groups.get(key);
    if (list) list.push(m);
    else groups.set(key, [m]);
  }
  const order = [...GROUP_KEYWORDS, 'other'];
  return order.filter((k) => groups.has(k)).map((k) => ({ key: k, metrics: groups.get(k)! }));
}

function hasVisible(node: MetricNode, visibleIds: ReadonlySet<string>): boolean {
  if (node.level === 'metric') return visibleIds.has(node.id);
  return (node.children ?? []).some((c) => hasVisible(c, visibleIds));
}

interface TreeNodeRowProps {
  node: MetricNode;
  disabled: boolean;
  visibleIds: ReadonlySet<string>;
  favorites: ReadonlySet<string>;
  onToggle: (node: MetricNode, checked: boolean) => void;
  onToggleFavorite: (id: string) => void;
}

function TreeNodeRow({ node, disabled, visibleIds, favorites, onToggle, onToggleFavorite }: TreeNodeRowProps) {
  const { state } = useSession();
  const { t } = useTranslation();

  if (node.level === 'metric') {
    if (!visibleIds.has(node.id)) return null;
    const checked = state.selectedMetrics.has(node.id);
    const fav = favorites.has(node.id);
    return (
      <li className={`metric-tree__node metric-tree__node--metric${disabled ? ' metric-tree__node--disabled' : ''}`}>
        <div className="metric-tree__row">
          <label className="metric-tree__label">
            <input
              type="checkbox"
              checked={checked}
              disabled={disabled}
              onChange={(e) => onToggle(node, e.target.checked)}
            />
            <span className="metric-tree__name">{node.name}</span>
            {node.unit && <span className="metric-tree__unit">{node.unit}</span>}
          </label>
          {/* P2-01：收藏星标（☆/★），点击切换，persist 到 localStorage。 */}
          <button
            type="button"
            className={`metric-tree__star${fav ? ' metric-tree__star--on' : ''}`}
            aria-label={t('workbench.metrics.fav_toggle', { defaultValue: '收藏' })}
            aria-pressed={fav}
            onClick={() => onToggleFavorite(node.id)}
          >
            {fav ? '★' : '☆'}
          </button>
        </div>
        {node.description && <span className="metric-tree__tip">{node.description}</span>}
        {node.aggregation && (
          <span className="metric-tree__tip">
            {t('workbench.metrics.aggregation', { agg: t(`workbench.metrics.agg_${node.aggregation}`) })}
          </span>
        )}
      </li>
    );
  }

  // 非叶节点（文件/插件）：过滤掉当前检索/收藏视图下不可见的后代；全不可见则整行隐藏。
  const childNodes = (node.children ?? []).filter((child) => hasVisible(child, visibleIds));
  if (childNodes.length === 0) return null;

  const metricIds = collectMetricIds(node);
  const checkedCount = metricIds.filter((id) => state.selectedMetrics.has(id)).length;
  const allChecked = checkedCount === metricIds.length && metricIds.length > 0;

  // 非叶节点（文件/插件）半选联动：子全选 → checked；子部分选 → indeterminate（半选）；子全不选 → 未选。
  // ref 回调在每次渲染的 commit 阶段按当前选中态写入 DOM 的 indeterminate 属性。
  const setIndeterminate = (el: HTMLInputElement | null) => {
    if (el) el.indeterminate = checkedCount > 0 && checkedCount < metricIds.length;
  };

  return (
    <li className={`metric-tree__node${disabled ? ' metric-tree__node--disabled' : ''}`}>
      <label className="metric-tree__label">
        <input
          type="checkbox"
          checked={allChecked}
          disabled={disabled}
          ref={setIndeterminate}
          onChange={(e) => onToggle(node, e.target.checked)}
        />
        <span className="metric-tree__name">
          {node.level === 'file' && node.children?.[0]
            ? fileDisplayName(node.file_id, node.name, state.files)
            : node.name}
        </span>
      </label>
      {node.level === 'plugin' ? (
        // P2-01：插件内按指标名关键词语义分组（cpu/gpu/mem/…，无命中落「其他」）。
        <ul className="metric-tree__children">
          {partitionByGroup(childNodes as MetricNode[]).map((group) => (
            <li key={group.key} className="metric-tree__group">
              <h3 className="metric-tree__group-title">
                {group.key === 'other'
                  ? t('workbench.metrics.group_other', { defaultValue: '其他' })
                  : group.key}
              </h3>
              <ul className="metric-tree__children">
                {group.metrics.map((child) => (
                  <TreeNodeRow
                    key={child.id}
                    node={child}
                    disabled={disabled}
                    visibleIds={visibleIds}
                    favorites={favorites}
                    onToggle={onToggle}
                    onToggleFavorite={onToggleFavorite}
                  />
                ))}
              </ul>
            </li>
          ))}
        </ul>
      ) : (
        <ul className="metric-tree__children">
          {childNodes.map((child) => (
            <TreeNodeRow
              key={child.id}
              node={child}
              disabled={disabled}
              visibleIds={visibleIds}
              favorites={favorites}
              onToggle={onToggle}
              onToggleFavorite={onToggleFavorite}
            />
          ))}
        </ul>
      )}
    </li>
  );
}

/** Three-level checkbox tree (file → plugin → metric) with half-checked linkage, disabled greying (§4.3),
 *  plus P2-01: instant search, favorites (☆/★ + localStorage), recent usage and semantic grouping. */
export default function MetricTree() {
  const { state, actions } = useSession();
  const { t } = useTranslation();

  const [query, setQuery] = useState('');
  const [favOnly, setFavOnly] = useState(false);
  const [favorites, setFavorites] = useState<Set<string>>(() => readStoredIds(FAVORITES_KEY));
  const [recent, setRecent] = useState<string[]>(() => readStoredRecent(RECENT_KEY));

  /** 复合 id → 指标节点（最近使用分区按当前树解析展示名；树外条目自动跳过）。 */
  const metricById = useMemo(() => {
    const map = new Map<string, MetricNode>();
    const walk = (n: MetricNode) => {
      if (n.level === 'metric') map.set(n.id, n);
      for (const c of n.children ?? []) walk(c);
    };
    for (const f of state.metricTree) walk(f);
    return map;
  }, [state.metricTree]);

  /** 当前检索（名称/单位/描述，大小写不敏感）与「只看收藏」过滤下的可见指标集。 */
  const visibleIds = useMemo(() => {
    const q = query.trim().toLowerCase();
    const ids = new Set<string>();
    const walk = (n: MetricNode) => {
      if (n.level === 'metric') {
        if (favOnly && !favorites.has(n.id)) return;
        if (q !== '' && !`${n.name} ${n.unit ?? ''} ${n.description ?? ''}`.toLowerCase().includes(q)) return;
        ids.add(n.id);
        return;
      }
      for (const c of n.children ?? []) walk(c);
    };
    for (const f of state.metricTree) walk(f);
    return ids;
  }, [state.metricTree, query, favOnly, favorites]);

  const toggleFavorite = (id: string) => {
    setFavorites((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      writeStoredIds(FAVORITES_KEY, [...next]);
      return next;
    });
  };

  /** 勾选/取消指标即记录最近使用：去重、最新在前、上限 10 条。 */
  const recordRecent = (id: string) => {
    setRecent((prev) => {
      const next = [id, ...prev.filter((x) => x !== id)].slice(0, RECENT_LIMIT);
      writeStoredIds(RECENT_KEY, next);
      return next;
    });
  };

  const toggle = (node: MetricNode, checked: boolean) => {
    if (node.level === 'metric') {
      actions.toggleMetrics([node.id], checked);
      recordRecent(node.id);
      return;
    }
    const ids = collectMetricIds(node);
    actions.toggleMetrics(ids, checked);
    for (const id of ids) recordRecent(id);
  };

  const searching = query.trim() !== '';
  const noMatch = state.metricTree.length > 0 && visibleIds.size === 0;
  const recentItems = recent.map((id) => metricById.get(id)).filter((n): n is MetricNode => Boolean(n));

  return (
    <section className="metric-tree">
      <h2 className="metric-tree__title">{t('workbench.metrics.title')}</h2>
      {/* Wave 4 C10：场景预设工具条（面板 header 区域；本地 state，不进 SessionContext）。 */}
      <PresetBar />
      {state.metricTree.length === 0 ? (
        <p className="metric-tree__empty">{t('workbench.metrics.empty')}</p>
      ) : (
        <>
          <div className="metric-tree__toolbar">
            <input
              type="search"
              className="metric-tree__search"
              aria-label={t('workbench.metrics.search_label', { defaultValue: '搜索指标' })}
              placeholder={t('workbench.metrics.search_placeholder', { defaultValue: '搜索名称/单位/描述' })}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            {/* 用 role=checkbox 按钮而非 <input type=checkbox>：既有用例（real-import-flow/
                viewport-fit/cursor-zr-click）以 querySelector('input[type=checkbox]') 取
                文档首个复选框，新增原生复选框会把它们指向过滤开关而破坏原语义。 */}
            <button
              type="button"
              role="checkbox"
              aria-checked={favOnly}
              className={`metric-tree__fav-only${favOnly ? ' metric-tree__fav-only--on' : ''}`}
              onClick={() => setFavOnly((v) => !v)}
            >
              <span className="metric-tree__fav-only-box">{favOnly ? '☑' : '☐'}</span>
              <span>{t('workbench.metrics.fav_only', { defaultValue: '只看收藏' })}</span>
            </button>
          </div>
          {!searching && !favOnly && recentItems.length > 0 && (
            <section
              className="metric-tree__recent"
              aria-label={t('workbench.metrics.recent', { defaultValue: '最近使用' })}
            >
              <h3 className="metric-tree__recent-title">{t('workbench.metrics.recent', { defaultValue: '最近使用' })}</h3>
              <ul className="metric-tree__recent-list">
                {recentItems.map((n) => {
                  const checked = state.selectedMetrics.has(n.id);
                  return (
                    <li key={n.id}>
                      {/* 用 aria-pressed 按钮而非 checkbox：避免与树内同名指标的复选框在
                          getByRole 查询下产生歧义（FilePanel/snapshot 既有用例依赖唯一匹配）。 */}
                      <button
                        type="button"
                        className={`metric-tree__recent-item${checked ? ' metric-tree__recent-item--on' : ''}`}
                        aria-pressed={checked}
                        onClick={() => {
                          actions.toggleMetrics([n.id], !checked);
                          recordRecent(n.id);
                        }}
                      >
                        <span className="metric-tree__name">{n.name}</span>
                        {n.unit && <span className="metric-tree__unit">{n.unit}</span>}
                      </button>
                    </li>
                  );
                })}
              </ul>
            </section>
          )}
          {noMatch ? (
            <p className="metric-tree__empty">{t('workbench.metrics.no_match', { defaultValue: '无匹配指标' })}</p>
          ) : (
            <>
              {state.selectedMetrics.size === 0 && <p className="metric-tree__hint">{t('workbench.metrics.select_hint')}</p>}
              <ul className="metric-tree__root">
                {state.metricTree.map((fileNode) => (
                  <TreeNodeRow
                    key={fileNode.id}
                    node={fileNode}
                    disabled={state.disabledFiles.has(fileNode.file_id)}
                    visibleIds={visibleIds}
                    favorites={favorites}
                    onToggle={toggle}
                    onToggleFavorite={toggleFavorite}
                  />
                ))}
              </ul>
            </>
          )}
        </>
      )}
    </section>
  );
}

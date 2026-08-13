import type { MetricNode } from '../ipc/types';
import { useSession } from '../state/session';
import { useTranslation } from 'react-i18next';
import './MetricTree.css';

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

interface TreeNodeRowProps {
  node: MetricNode;
  disabled: boolean;
  onToggle: (node: MetricNode, checked: boolean) => void;
}

function TreeNodeRow({ node, disabled, onToggle }: TreeNodeRowProps) {
  const { state } = useSession();
  const { t } = useTranslation();

  if (node.level === 'metric') {
    const checked = state.selectedMetrics.has(node.id);
    return (
      <li className={`metric-tree__node metric-tree__node--metric${disabled ? ' metric-tree__node--disabled' : ''}`}>
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
        {node.description && <span className="metric-tree__tip">{node.description}</span>}
        {node.aggregation && (
          <span className="metric-tree__tip">
            {t('workbench.metrics.aggregation', { agg: t(`workbench.metrics.agg_${node.aggregation}`) })}
          </span>
        )}
      </li>
    );
  }

  const childNodes = node.children ?? [];
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
      {childNodes.length > 0 && (
        <ul className="metric-tree__children">
          {childNodes.map((child) => (
            <TreeNodeRow key={child.id} node={child} disabled={disabled} onToggle={onToggle} />
          ))}
        </ul>
      )}
    </li>
  );
}

/** Three-level checkbox tree (file → plugin → metric) with half-checked linkage and disabled greying (§4.3). */
export default function MetricTree() {
  const { state, actions } = useSession();
  const { t } = useTranslation();

  const toggle = (node: MetricNode, checked: boolean) => {
    if (node.level === 'metric') {
      actions.toggleMetrics([node.id], checked);
      return;
    }
    actions.toggleMetrics(collectMetricIds(node), checked);
  };

  return (
    <section className="metric-tree">
      <h2 className="metric-tree__title">{t('workbench.metrics.title')}</h2>
      {state.metricTree.length === 0 ? (
        <p className="metric-tree__empty">{t('workbench.metrics.empty')}</p>
      ) : (
        <>
          {state.selectedMetrics.size === 0 && <p className="metric-tree__hint">{t('workbench.metrics.select_hint')}</p>}
          <ul className="metric-tree__root">
            {state.metricTree.map((fileNode) => (
              <TreeNodeRow
                key={fileNode.id}
                node={fileNode}
                disabled={state.disabledFiles.has(fileNode.file_id)}
                onToggle={toggle}
              />
            ))}
          </ul>
        </>
      )}
    </section>
  );
}

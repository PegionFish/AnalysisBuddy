/** ui/src/components/RecoverySummary.test.tsx — P1-03 会话恢复摘要（TDD）：
 *  混合恢复态渲染「已恢复 X/Y + 缺失/重开失败汇总」；展开后逐项原因文案；
 *  重试重新导入（reopen_failed/missing）；复制诊断写 clipboard；关闭后消失。 */

import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ipc } from '../ipc/ipc';
import type { ImportResult } from '../ipc/types';
import RecoverySummary from './RecoverySummary';
import { SessionProvider, useSession, type SessionAction, type SessionState } from '../state/session';

interface ProbeApi {
  state: SessionState | null;
  dispatch: React.Dispatch<SessionAction> | null;
}

/** 状态探针：把 SessionState/dispatch 暴露给断言（session.snapshot.test 同风格）。 */
function StateProbe({ api }: { api: ProbeApi }) {
  const { state, dispatch } = useSession();
  api.state = state;
  api.dispatch = dispatch;
  return null;
}

function ready(fileId: string, path: string): ImportResult {
  return {
    file_id: fileId,
    path,
    name: path.split(/[\\/]/).pop() ?? path,
    size_bytes: 100,
    status: 'ready',
    matched_plugin: { plugin_id: 'builtin-csv', confidence: 1 },
    candidate_plugins: [],
    time_range: { start_ms: 0, end_ms: 600_000 },
  };
}

function renderSummary(api: ProbeApi) {
  return render(
    <SessionProvider>
      <StateProbe api={api} />
      <RecoverySummary />
    </SessionProvider>,
  );
}

/** 混合状态：2 ready + 1 missing(not_found) + 1 reopen_failed（Y=4，X=2）。 */
function seedMixed(api: ProbeApi) {
  renderSummary(api);
  act(() => {
    api.dispatch!({
      type: 'files/imported',
      results: [ready('f1', 'C:\\data\\a.csv'), ready('f2', 'C:\\data\\b.csv')],
    });
    api.dispatch!({
      type: 'session/missing',
      entries: [{ path: 'C:\\data\\gone.csv', reason: 'not_found' }],
    });
    api.dispatch!({
      type: 'session/reopen_failed',
      entries: [{ path: 'C:\\data\\busy.csv', reason: 'reopen_failed' }],
    });
  });
}

async function advance(ms: number): Promise<void> {
  const step = 250;
  for (let t = 0; t < ms; t += step) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(Math.min(step, ms - t));
    });
  }
}

describe('RecoverySummary (P1-03)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    Reflect.deleteProperty(navigator, 'clipboard');
  });

  it('无失败条目时不渲染（null）', () => {
    const api: ProbeApi = { state: null, dispatch: null };
    renderSummary(api);
    expect(screen.queryByTestId('recovery-summary')).not.toBeInTheDocument();
  });

  it('混合状态渲染摘要：已恢复 X/Y + 缺失/重开失败汇总（role=status）', () => {
    const api: ProbeApi = { state: null, dispatch: null };
    seedMixed(api);
    expect(screen.getByTestId('recovery-summary')).toBeInTheDocument();
    const head = screen.getByTestId('recovery-summary-head');
    expect(head).toHaveAttribute('role', 'status');
    expect(head).toHaveTextContent('已恢复 2/4 个文件');
    expect(head).toHaveTextContent('1 个缺失，1 个重开失败');
  });

  it('展开后逐项列出失败条目：路径 title 完整路径 + 原因文案（role=alert）', () => {
    const api: ProbeApi = { state: null, dispatch: null };
    seedMixed(api);
    expect(screen.queryByTestId('recovery-failures')).not.toBeInTheDocument();

    fireEvent.click(screen.getByTestId('recovery-toggle'));
    const list = screen.getByTestId('recovery-failures');
    expect(list).toHaveAttribute('role', 'alert');
    const items = within(list).getAllByTestId('recovery-failure');
    expect(items).toHaveLength(2);

    const gone = items.find((el) => el.textContent?.includes('gone.csv'))!;
    expect(gone).toBeTruthy();
    expect(within(gone).getByTestId('recovery-path')).toHaveAttribute('title', 'C:\\data\\gone.csv');
    expect(within(gone).getByTestId('recovery-reason')).toHaveTextContent('文件缺失');

    const busy = items.find((el) => el.textContent?.includes('busy.csv'))!;
    expect(busy).toBeTruthy();
    expect(within(busy).getByTestId('recovery-path')).toHaveAttribute('title', 'C:\\data\\busy.csv');
    expect(within(busy).getByTestId('recovery-reason')).toHaveTextContent('重新解析失败');
  });

  it('三种原因文案映射：文件缺失 / 内容已变更 / 重新解析失败', () => {
    const api: ProbeApi = { state: null, dispatch: null };
    renderSummary(api);
    act(() => {
      api.dispatch!({
        type: 'session/missing',
        entries: [
          { path: 'C:\\data\\gone.csv', reason: 'not_found' },
          { path: 'C:\\data\\changed.csv', reason: 'hash_mismatch' },
        ],
      });
      api.dispatch!({
        type: 'session/reopen_failed',
        entries: [{ path: 'C:\\data\\busy.csv', reason: 'reopen_failed' }],
      });
    });
    fireEvent.click(screen.getByTestId('recovery-toggle'));
    expect(screen.getByText('文件缺失')).toBeInTheDocument();
    expect(screen.getByText('内容已变更')).toBeInTheDocument();
    expect(screen.getByText('重新解析失败')).toBeInTheDocument();
  });

  it('reopen_failed 条目重试：调用 importFiles([path]) 重新导入', async () => {
    const spy = vi.spyOn(ipc, 'import_files');
    const api: ProbeApi = { state: null, dispatch: null };
    seedMixed(api);
    fireEvent.click(screen.getByTestId('recovery-toggle'));
    const items = within(screen.getByTestId('recovery-failures')).getAllByTestId('recovery-failure');
    const busy = items.find((el) => el.textContent?.includes('busy.csv'))!;
    fireEvent.click(within(busy).getByTestId('recovery-retry'));
    await advance(300);

    expect(spy).toHaveBeenCalledWith(expect.objectContaining({ paths: ['C:\\data\\busy.csv'] }));
  });

  it('missing 条目重试：同样重新导入，并提示文件需存在（title）', async () => {
    const spy = vi.spyOn(ipc, 'import_files');
    const api: ProbeApi = { state: null, dispatch: null };
    seedMixed(api);
    fireEvent.click(screen.getByTestId('recovery-toggle'));
    const items = within(screen.getByTestId('recovery-failures')).getAllByTestId('recovery-failure');
    const gone = items.find((el) => el.textContent?.includes('gone.csv'))!;
    const busy = items.find((el) => el.textContent?.includes('busy.csv'))!;

    expect(within(gone).getByTestId('recovery-retry')).toHaveAttribute('title', expect.stringContaining('存在'));
    expect(within(busy).getByTestId('recovery-retry')).not.toHaveAttribute('title');

    fireEvent.click(within(gone).getByTestId('recovery-retry'));
    await advance(300);
    expect(spy).toHaveBeenCalledWith(expect.objectContaining({ paths: ['C:\\data\\gone.csv'] }));
  });

  it('复制诊断信息：clipboard.writeText 写入 路径+原因+时间戳', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    const api: ProbeApi = { state: null, dispatch: null };
    seedMixed(api);
    fireEvent.click(screen.getByTestId('recovery-toggle'));
    const items = within(screen.getByTestId('recovery-failures')).getAllByTestId('recovery-failure');
    const gone = items.find((el) => el.textContent?.includes('gone.csv'))!;

    fireEvent.click(within(gone).getByTestId('recovery-copy'));
    await advance(10);

    expect(writeText).toHaveBeenCalledTimes(1);
    const text = writeText.mock.calls[0][0] as string;
    expect(text).toContain('C:\\data\\gone.csv');
    expect(text).toContain('文件缺失');
    expect(text).toMatch(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}/);
  });

  it('关闭按钮（dismiss）：摘要整体消失', () => {
    const api: ProbeApi = { state: null, dispatch: null };
    seedMixed(api);
    expect(screen.getByTestId('recovery-summary')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('recovery-dismiss'));
    expect(screen.queryByTestId('recovery-summary')).not.toBeInTheDocument();
  });
});

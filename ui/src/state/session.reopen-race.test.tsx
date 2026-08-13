/** ui/src/state/session.reopen-race.test.tsx — P0-01 竞态回归（真实 Tauri
 *  事件顺序）：后端 `load_session` 内部 await 完整重放，`percent:100` 进度
 *  事件在**响应返回之前**即已发出。旧契约下前端拿到响应后才挂占位行，
 *  事件先行 → 永远“解析中”；新契约下 `LoadResult.files` 携带 ready 终态行，
 *  前端直接写终态，不依赖事件时序。
 *
 *  本测试显式注入“响应前到达”的进度事件 + 断言 rows 直接 ready：
 *  若未来回退到占位行契约（或 mock 恢复旧行为），本测试即失败。 */

import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { EV_PROGRESS } from '../ipc/events';
import { ipc } from '../ipc/ipc';
import type { ProgressPayload } from '../ipc/events';
import FilePanel from '../components/FilePanel';
import MetricTree from '../components/MetricTree';
import TopBar from '../components/TopBar';
import { SessionProvider, useSession, type SessionState } from './session';

interface ProbeApi {
  state: SessionState | null;
}

/** 状态探针：把 SessionState 暴露给断言（session.snapshot.test 同风格）。 */
function StateProbe({ api }: { api: ProbeApi }) {
  const { state } = useSession();
  api.state = state;
  return null;
}

function renderWorkbench(api: ProbeApi) {
  return render(
    <SessionProvider>
      <StateProbe api={api} />
      <TopBar route="/" onNavigate={vi.fn()} />
      <FilePanel />
      <MetricTree />
    </SessionProvider>,
  );
}

async function advance(ms: number): Promise<void> {
  const step = 50;
  for (let t = 0; t < ms; t += step) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(Math.min(step, ms - t));
    });
  }
}

describe('P0-01 会话重开竞态（真实事件顺序）', () => {
  /** 捕获 SessionProvider 注册的 EV_PROGRESS 监听器（用于注入提前到达的事件）。 */
  const progressListeners: Array<(p: ProgressPayload) => void> = [];

  beforeEach(() => {
    vi.useFakeTimers();
    progressListeners.length = 0;
    const origListen = ipc.listen.bind(ipc);
    vi.spyOn(ipc, 'listen').mockImplementation((channel: string, cb: unknown) => {
      const off = origListen(channel, cb as never);
      if (channel === EV_PROGRESS) progressListeners.push(cb as (p: ProgressPayload) => void);
      return off;
    });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  async function seedAndSaveSession(): Promise<{ path: string; fileId: string }> {
    fireEvent.change(screen.getByTestId('path-input'), { target: { value: 'C:\\data\\race.csv' } });
    fireEvent.click(screen.getByRole('button', { name: 'Import Files' }));
    await advance(10_500);
    expect(screen.getByTestId('status-badge')).toHaveTextContent('Ready');

    fireEvent.click(screen.getByRole('button', { name: 'Save Session' }));
    await advance(500);
    const saved = JSON.parse(localStorage.getItem('ab.mock.session')!) as {
      path: string;
      files: { file_id: string }[];
    };
    expect(saved.files).toHaveLength(1);

    fireEvent.click(screen.getByRole('button', { name: 'New Session' }));
    await advance(300);
    return { path: saved.path, fileId: saved.files[0].file_id };
  }

  it('响应前到达的 percent:100 事件不得把文件卡在“解析中”；rows 由响应直接 ready', async () => {
    const api: ProbeApi = { state: null };
    renderWorkbench(api);
    const { path: sessionPath, fileId } = await seedAndSaveSession();

    // 打开会话：load_session 在途（mock 延迟 40-150ms，尚未返回）时，
    // 后端重放事件（percent:100）已经到达前端——真实 Tauri 事件顺序。
    fireEvent.change(screen.getByRole('textbox', { name: 'Session file path' }), {
      target: { value: sessionPath },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Open Session' }));
    // 在 load_session 返回前推进一小段时间，让“竞态事件”先行到达。
    await advance(30);
    expect(progressListeners.length).toBeGreaterThan(0);
    act(() => {
      for (const handler of progressListeners) {
        handler({ file_id: fileId, percent: 100, records_so_far: 600 });
      }
    });

    // load_session 返回：rows 必须由响应内的 ready 终态行直接写入。
    await advance(1_000);
    const entry = screen.getByTestId('file-entry');
    expect(entry).toHaveAttribute('data-file-id', fileId);
    expect(screen.getByTestId('status-badge')).toHaveTextContent('Ready');
    // 不得残留“解析中”占位（旧契约症状：永远 parsing + {{records}} 泄漏）。
    expect(screen.queryByTestId('progress')).not.toBeInTheDocument();
    expect(screen.getByTestId('status-badge').textContent).not.toContain('{{');
  });

  it('不注入任何进度事件时打开会话依然直接 ready（不依赖事件到达）', async () => {
    const api: ProbeApi = { state: null };
    renderWorkbench(api);
    const { path: sessionPath } = await seedAndSaveSession();

    fireEvent.change(screen.getByRole('textbox', { name: 'Session file path' }), {
      target: { value: sessionPath },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Open Session' }));
    await advance(1_000);

    expect(screen.getByTestId('status-badge')).toHaveTextContent('Ready');
    expect(screen.queryByTestId('progress')).not.toBeInTheDocument();

    // 恢复后插件列表显式重取（session/reset 清空 plugins 后不得显示
    // “暂无插件”——P1-03/报告建议 3）。
    await advance(300);
    expect(api.state!.plugins.length).toBeGreaterThan(0);
  });});

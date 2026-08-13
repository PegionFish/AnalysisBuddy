import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { SessionProvider } from '../state/session';
import FilePanel from './FilePanel';
import MetricTree from './MetricTree';
import TimelineChart from './TimelineChart';
import TopBar from './TopBar';

interface CapturedOption {
  animation: boolean;
  series: Array<{ name: string; data: unknown[] }>;
}

const echartsMock = vi.hoisted(() => {
  const calls: { option: CapturedOption }[] = [];
  const instance = {
    setOption: (option: unknown) => {
      calls.push({ option: option as CapturedOption });
    },
    on: () => undefined,
    // 任务 23：游标走 zrender 层 click（series 级 click 在 large+symbol:none 下永不触发）。
    getZr: () => ({ on: () => undefined }),
    containPixel: () => false,
    convertFromPixel: () => NaN,
    dispose: () => undefined,
    resize: () => undefined,
    clear: () => undefined,
    getOption: () => ({}),
  };
  return { calls, instance, init: vi.fn(() => instance) };
});

vi.mock('echarts', () => ({
  init: echartsMock.init,
  use: vi.fn(),
}));

function renderWorkbench() {
  return render(
    <SessionProvider>
      <TopBar route="/" onNavigate={vi.fn()} />
      <FilePanel />
      <MetricTree />
      <TimelineChart />
    </SessionProvider>,
  );
}

async function advance(ms: number): Promise<void> {
  const step = 250;
  for (let t = 0; t < ms; t += step) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(Math.min(step, ms - t));
    });
  }
}

describe('openSession restores the workbench (ipc-ui.md §4.1/§4.2)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    echartsMock.calls.length = 0;
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('load_session 响应携带 ready 终态行：打开后直接 Ready，指标/曲线可渲染', async () => {
    renderWorkbench();

    fireEvent.change(screen.getByTestId('path-input'), { target: { value: 'C:\\data\\restore.csv' } });
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
    expect(screen.queryByTestId('file-entry')).not.toBeInTheDocument();
    expect(screen.getByText('No files yet')).toBeInTheDocument();

    fireEvent.change(screen.getByRole('textbox', { name: 'Session file path' }), {
      target: { value: saved.path },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Open Session' }));
    await advance(500);

    // P0-01：后端已 await 完整重放，rows 由响应直接写入 ready 终态——
    // 不得出现“永远解析中”占位（旧契约靠重放进度事件翻转，事件在
    // 响应前已发出，占位行永远收不到）。
    const entry = screen.getByTestId('file-entry');
    expect(entry).toHaveAttribute('data-file-id', saved.files[0].file_id);
    expect(screen.getByTestId('status-badge')).toHaveTextContent('Ready');
    expect(screen.queryByTestId('progress')).not.toBeInTheDocument();

    const metricBox = screen.getByRole('checkbox', { name: /metric_1/ });
    fireEvent.click(metricBox);
    await advance(1_500);

    const last = echartsMock.calls[echartsMock.calls.length - 1];
    expect(last).toBeDefined();
    expect(last.option.series.length).toBeGreaterThanOrEqual(1);
    expect(last.option.series[0].name).toMatch(/restore/);
  });
});

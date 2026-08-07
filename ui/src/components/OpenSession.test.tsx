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

  it('replays loaded files through the import pipeline: rows appear, drive to ready, metrics and series render', async () => {
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

    const entry = screen.getByTestId('file-entry');
    expect(entry).toHaveAttribute('data-file-id', saved.files[0].file_id);
    expect(screen.getByTestId('status-badge')).toHaveTextContent(/Parsing/);
    expect(screen.getByTestId('progress')).toBeInTheDocument();

    await advance(10_000);
    expect(screen.getByTestId('status-badge')).toHaveTextContent('Ready');

    const metricBox = screen.getByRole('checkbox', { name: /metric_1/ });
    fireEvent.click(metricBox);
    await advance(1_500);

    const last = echartsMock.calls[echartsMock.calls.length - 1];
    expect(last).toBeDefined();
    expect(last.option.series.length).toBeGreaterThanOrEqual(1);
    expect(last.option.series[0].name).toMatch(/restore/);
  });
});

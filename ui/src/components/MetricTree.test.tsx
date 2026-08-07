import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ipc } from '../ipc/ipc';
import { SessionProvider } from '../state/session';
import FilePanel from './FilePanel';
import MetricTree from './MetricTree';

function renderTree() {
  return render(
    <SessionProvider>
      <FilePanel />
      <MetricTree />
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

/** Import a csv and wait for the ready state + metric tree refresh. */
async function setupReadyFile(path: string): Promise<void> {
  fireEvent.change(screen.getByTestId('path-input'), { target: { value: path } });
  fireEvent.click(screen.getByRole('button', { name: 'Import Files' }));
  await advance(500);
  await advance(10_000);
}

describe('MetricTree (ipc-ui.md §4.3)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('renders file → plugin → metric rows for ready files', async () => {
    renderTree();
    await setupReadyFile('C:\\data\\tree.csv');

    const boxes = screen.getAllByRole('checkbox');
    expect(boxes.length).toBeGreaterThan(0);
    expect(screen.getAllByText('tree.csv').length).toBeGreaterThan(0);
    expect(screen.getByText('Builtin CSV')).toBeInTheDocument();
    expect(screen.getByRole('checkbox', { name: /metric_1/ })).toBeInTheDocument();
    expect(screen.getByRole('checkbox', { name: /metric_2/ })).toBeInTheDocument();
    expect(screen.getAllByText(/Aggregation:/).length).toBeGreaterThan(0);
  });

  it('toggles a metric and fires query_series for the current window', async () => {
    const spy = vi.spyOn(ipc, 'query_series');
    renderTree();
    await setupReadyFile('C:\\data\\query.csv');

    const metricBox = screen.getByRole('checkbox', { name: /metric_1/ });
    fireEvent.click(metricBox);
    await advance(1_000);

    expect(spy).toHaveBeenCalledTimes(1);
    const args = spy.mock.calls[0][0];
    expect(args.metrics).toHaveLength(1);
    expect(args.metrics[0]).toMatch(/^mock-.+builtin-csv:metric-1$/);
    expect(args.t0_ms).toBe(0);
    expect(args.t1_ms).toBe(600_000);
    expect(args.max_points_per_series).toBe(4000);
    expect(metricBox).toBeChecked();
  });

  it('parent file checkbox checks all descendants and unchecks on second click', async () => {
    renderTree();
    await setupReadyFile('C:\\data\\parent.csv');

    const fileBox = screen.getByRole('checkbox', { name: /parent\.csv/ });
    const metricBoxes = screen.getAllByRole('checkbox', { name: /metric_\d/ });

    fireEvent.click(fileBox);
    expect(fileBox).toBeChecked();
    for (const box of metricBoxes) expect(box).toBeChecked();

    fireEvent.click(fileBox);
    expect(fileBox).not.toBeChecked();
    for (const box of metricBoxes) expect(box).not.toBeChecked();
  });

  it('greys out and disables checkboxes of disabled files', async () => {
    renderTree();
    await setupReadyFile('C:\\data\\disabled.csv');

    const metricBox = screen.getByRole('checkbox', { name: /metric_1/ });
    fireEvent.click(metricBox);
    await advance(1_000);

    fireEvent.click(screen.getByRole('button', { name: 'Disable' }));
    expect(metricBox).toBeDisabled();
    expect(metricBox).not.toBeChecked();
  });
});

import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ipc } from '../ipc/ipc';
import { SessionProvider } from '../state/session';
import FilePanel from './FilePanel';
import MetricTree from './MetricTree';

function renderPanel() {
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

async function importPath(path: string): Promise<void> {
  fireEvent.change(screen.getByTestId('path-input'), { target: { value: path } });
  fireEvent.click(screen.getByRole('button', { name: 'Import Files' }));
  await advance(500);
}

describe('FilePanel (ipc-ui.md §4.2)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('imports via the mock path input and shows parsing then ready with progress', async () => {
    renderPanel();

    await importPath('C:\\data\\run-1.csv');
    const entry = screen.getByTestId('file-entry');
    expect(within(entry).getByText('run-1.csv')).toBeInTheDocument();
    expect(within(entry).getByTestId('status-badge')).toHaveTextContent(/Parsing/);
    expect(within(entry).getByTestId('progress')).toBeInTheDocument();
    expect(within(entry).getAllByText(/builtin-csv/).length).toBeGreaterThan(0);

    await advance(10_000);
    expect(within(entry).getByTestId('status-badge')).toHaveTextContent('Ready');
  });

  it('shows error entries for fail-load paths and retry re-sends import_files([path])', async () => {
    const spy = vi.spyOn(ipc, 'import_files');
    renderPanel();

    await importPath('C:\\data\\fail-load.csv');
    const entry = screen.getByTestId('file-entry');
    expect(within(entry).getByTestId('entry-error')).toHaveTextContent('Failed to load file');

    spy.mockClear();
    fireEvent.click(within(entry).getByTestId('retry-btn'));
    await advance(500);
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy.mock.calls[0][0].paths).toEqual(['C:\\data\\fail-load.csv']);
    const retried = screen.getByTestId('file-entry');
    expect(within(retried).getByTestId('entry-error')).toBeInTheDocument();
  });

  it('unloads a file entry via unload_file', async () => {
    const spy = vi.spyOn(ipc, 'unload_file');
    renderPanel();

    await importPath('C:\\data\\bye.csv');
    await advance(10_000);
    expect(screen.getByTestId('file-entry')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('unload-btn'));
    await advance(500);
    expect(spy).toHaveBeenCalledWith({ file_id: expect.any(String) });
    expect(screen.queryByTestId('file-entry')).not.toBeInTheDocument();
    expect(screen.getByText('No files yet')).toBeInTheDocument();
  });

  it('disable toggle greys the entry and unchecks its metrics', async () => {
    renderPanel();

    await importPath('C:\\data\\metric.csv');
    await advance(10_000);

    const metricBox = screen.getByRole('checkbox', { name: /metric_1/ });
    expect(metricBox).not.toBeDisabled();
    fireEvent.click(metricBox);
    await advance(1_000);
    expect(metricBox).toBeChecked();

    const entry = screen.getByTestId('file-entry');
    fireEvent.click(within(entry).getByRole('button', { name: 'Disable' }));
    expect(entry).toHaveClass('file-entry--disabled');
    expect(screen.getByRole('checkbox', { name: /metric_1/ })).toBeDisabled();
    expect(screen.getByRole('checkbox', { name: /metric_1/ })).not.toBeChecked();

    fireEvent.click(within(entry).getByRole('button', { name: 'Enable' }));
    expect(entry).not.toHaveClass('file-entry--disabled');
  });

  it('needs_user_choice shows candidate buttons and re-imports with overrides on pick', async () => {
    const spy = vi.spyOn(ipc, 'import_files');
    renderPanel();

    await importPath('C:\\data\\choice.dat');
    const entry = screen.getByTestId('file-entry');
    expect(within(entry).getByTestId('plugin-choice')).toBeInTheDocument();

    fireEvent.click(within(entry).getByRole('button', { name: /builtin-csv/ }));
    expect(spy).toHaveBeenCalledWith({
      paths: ['C:\\data\\choice.dat'],
      overrides: { 'C:\\data\\choice.dat': { plugin_id: 'builtin-csv' } },
    });
    await advance(500);
    expect(within(entry).getByTestId('status-badge')).toHaveTextContent(/Parsing/);
  });
});

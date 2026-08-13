import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ipc } from '../ipc/ipc';
import type { KeyValueResult } from '../ipc/types';
import { KEYVALUES_DEBOUNCE_MS, SessionProvider, useSession } from '../state/session';
import FilePanel from './FilePanel';
import KeyValuesPanel from './KeyValuesPanel';

/** Test harness: drive cursor/set dispatches the way TimelineChart does via chart clicks. */
function SetCursor({ ms, label }: { ms: number | null; label: string }) {
  const { dispatch } = useSession();
  return (
    <button type="button" onClick={() => dispatch({ type: 'cursor/set', ms })}>
      {label}
    </button>
  );
}

function renderPanel() {
  return render(
    <SessionProvider>
      <SetCursor ms={120_000} label="cursor-120" />
      <SetCursor ms={150_000} label="cursor-150" />
      <SetCursor ms={200_000} label="cursor-200" />
      <SetCursor ms={null} label="cursor-none" />
      <FilePanel />
      <KeyValuesPanel />
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

/**
 * Two ready files with distinct key_values parity: the first import gets the `-odd` id
 * (mock §3.3 timeout injection), the second a normal id (get_metrics in between shifts the
 * parity deterministically).
 */
async function setupTwoFiles(): Promise<{ oddId: string; evenId: string }> {
  await importPath('C:\\data\\first.csv');
  await advance(10_000);
  await importPath('C:\\data\\second.csv');
  await advance(10_000);
  const entries = screen.getAllByTestId('file-entry');
  const odd = entries[0].getAttribute('data-file-id') ?? '';
  const even = entries[1].getAttribute('data-file-id') ?? '';
  expect(odd.endsWith('-odd')).toBe(true);
  expect(even.endsWith('-odd')).toBe(false);
  return { oddId: odd, evenId: even };
}

function groupById(id: string): HTMLElement {
  const group = screen.getAllByTestId('kv-group').find((g) => g.getAttribute('data-file-id') === id);
  if (!group) throw new Error(`kv-group ${id} not found`);
  return group;
}

describe('KeyValuesPanel (ipc-ui.md §4.5/§5.3)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('shows the no-cursor hint and never queries key_values_at without a cursor', async () => {
    const spy = vi.spyOn(ipc, 'key_values_at');
    renderPanel();
    expect(screen.getByText('Click the chart to set a cursor and inspect state at T')).toBeInTheDocument();

    await importPath('C:\\data\\idle.csv');
    await advance(10_000);
    expect(spy).not.toHaveBeenCalled();
  });

  it('debounces rapid cursor moves: a single trailing query with the final timestamp', async () => {
    const spy = vi.spyOn(ipc, 'key_values_at');
    renderPanel();
    await importPath('C:\\data\\db.csv');
    await advance(10_000);

    fireEvent.click(screen.getByRole('button', { name: 'cursor-120' }));
    await advance(150);
    fireEvent.click(screen.getByRole('button', { name: 'cursor-150' }));
    await advance(50);
    fireEvent.click(screen.getByRole('button', { name: 'cursor-200' }));
    await advance(KEYVALUES_DEBOUNCE_MS + 300);

    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy.mock.calls[0][0].timestamp_ms).toBe(200_000);
    expect(spy.mock.calls[0][0].file_ids).toHaveLength(1);
  });

  it('renders partial failure per group: -odd file shows timeout + retry, other files render entries', async () => {
    const spy = vi.spyOn(ipc, 'key_values_at');
    renderPanel();
    const { oddId, evenId } = await setupTwoFiles();

    fireEvent.click(screen.getByRole('button', { name: 'cursor-120' }));
    await advance(600);

    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy.mock.calls[0][0].file_ids).toEqual([oddId, evenId]);

    const odd = groupById(oddId);
    const even = groupById(evenId);
    expect(within(odd).getByTestId('kv-group-error')).toHaveTextContent('Query timed out for this file');
    expect(within(odd).getByTestId('kv-retry')).toBeInTheDocument();

    expect(within(even).queryByTestId('kv-group-error')).not.toBeInTheDocument();
    expect(within(even).getByText(/entries/)).toBeInTheDocument();
    expect(within(even).getByText('field_1')).toBeInTheDocument();
  });

  it('retries a single timed-out file via key_values_at([fileId]) and merges the result', async () => {
    const spy = vi.spyOn(ipc, 'key_values_at');
    renderPanel();
    const { oddId } = await setupTwoFiles();

    fireEvent.click(screen.getByRole('button', { name: 'cursor-120' }));
    await advance(600);
    expect(spy).toHaveBeenCalledTimes(1);

    fireEvent.click(within(groupById(oddId)).getByTestId('kv-retry'));
    await advance(600);

    expect(spy).toHaveBeenCalledTimes(2);
    expect(spy.mock.calls[1][0].file_ids).toEqual([oddId]);
    expect(spy.mock.calls[1][0].timestamp_ms).toBe(120_000);
    expect(screen.getAllByTestId('kv-group')).toHaveLength(2);
    expect(within(groupById(oddId)).getByTestId('kv-group-error')).toHaveTextContent(
      'Query timed out for this file',
    );
  });

  it('renders per-file error placeholders even when every file fails (never-reject consumption)', async () => {
    const spy = vi.spyOn(ipc, 'key_values_at');
    renderPanel();
    const { oddId, evenId } = await setupTwoFiles();

    spy.mockResolvedValue([
      { file_id: oddId, error: { code: 'timeout', message: 'timeout' } },
      { file_id: evenId, error: { code: 'plugin_crashed', message: 'crashed' } },
    ]);
    fireEvent.click(screen.getByRole('button', { name: 'cursor-120' }));
    await advance(600);

    expect(screen.getAllByTestId('kv-group')).toHaveLength(2);
    expect(within(groupById(oddId)).getByTestId('kv-group-error')).toHaveTextContent(
      'Query timed out for this file',
    );
    expect(within(groupById(evenId)).getByTestId('kv-group-error')).toHaveTextContent('Plugin crashed');
    expect(screen.queryByTestId('kv-loading')).not.toBeInTheDocument();
  });

  it('shows a friendly hint for a 0-key-values file instead of an empty Key/Value/Unit table header', async () => {
    const spy = vi.spyOn(ipc, 'key_values_at');
    renderPanel();
    const { oddId, evenId } = await setupTwoFiles();

    spy.mockResolvedValue([
      { file_id: oddId, entries: [{ key: 'mark', value: 'A' }] },
      { file_id: evenId, entries: [] },
    ]);
    fireEvent.click(screen.getByRole('button', { name: 'cursor-120' }));
    await advance(600);

    expect(spy).toHaveBeenCalledTimes(1);

    // Non-empty group keeps the real table (headers + rows).
    const withValues = groupById(oddId);
    expect(within(withValues).getByText('Key')).toBeInTheDocument();
    expect(within(withValues).getByText('mark')).toBeInTheDocument();
    expect(within(withValues).getByText('A')).toBeInTheDocument();
    expect(within(withValues).queryByTestId('kv-empty')).not.toBeInTheDocument();

    // 0-entry group renders the friendly hint and the count, but no empty table header.
    const empty = groupById(evenId);
    expect(within(empty).getByTestId('kv-empty')).toBeInTheDocument();
    expect(within(empty).getByText('The plugin did not provide key values')).toBeInTheDocument();
    expect(within(empty).getByText(/0 entries/)).toBeInTheDocument();
    expect(within(empty).queryByText('Key')).not.toBeInTheDocument();
    expect(within(empty).queryByText('Value')).not.toBeInTheDocument();
    expect(within(empty).queryByText('Unit')).not.toBeInTheDocument();
  });

  it('drops stale responses via seq: a late query for the previous cursor never overwrites the latest', async () => {
    const resolvers: Array<(r: KeyValueResult[]) => void> = [];
    vi.spyOn(ipc, 'key_values_at').mockImplementation(
      () => new Promise<KeyValueResult[]>((resolve) => resolvers.push(resolve)),
    );
    renderPanel();
    const { oddId, evenId } = await setupTwoFiles();

    fireEvent.click(screen.getByRole('button', { name: 'cursor-120' }));
    await advance(KEYVALUES_DEBOUNCE_MS + 200);
    expect(resolvers).toHaveLength(1);
    expect(screen.getByTestId('kv-loading')).toBeInTheDocument();

    act(() =>
      resolvers[0]([
        { file_id: oddId, error: { code: 'timeout', message: 'timeout' } },
        { file_id: evenId, entries: [{ key: 'mark', value: 'A' }] },
      ]),
    );
    await advance(100);
    expect(screen.queryByTestId('kv-loading')).not.toBeInTheDocument();
    expect(screen.getByText('A')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'cursor-200' }));
    await advance(KEYVALUES_DEBOUNCE_MS + 200);
    expect(resolvers).toHaveLength(2);
    expect(screen.getByText('A')).toBeInTheDocument();

    act(() =>
      resolvers[1]([
        { file_id: oddId, error: { code: 'timeout', message: 'timeout' } },
        { file_id: evenId, entries: [{ key: 'mark', value: 'B' }] },
      ]),
    );
    await advance(100);
    expect(screen.getByText('B')).toBeInTheDocument();
    expect(screen.queryByText('A')).not.toBeInTheDocument();

    act(() =>
      resolvers[0]([
        { file_id: oddId, error: { code: 'timeout', message: 'timeout' } },
        { file_id: evenId, entries: [{ key: 'mark', value: 'A2' }] },
      ]),
    );
    await advance(100);
    expect(screen.getByText('B')).toBeInTheDocument();
    expect(screen.queryByText('A2')).not.toBeInTheDocument();
  });

  it('clears the panel back to the no-cursor hint when the cursor is removed', async () => {
    renderPanel();
    await setupTwoFiles();

    fireEvent.click(screen.getByRole('button', { name: 'cursor-120' }));
    await advance(600);
    expect(screen.getAllByTestId('kv-group').length).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole('button', { name: 'cursor-none' }));
    expect(screen.getByText('Click the chart to set a cursor and inspect state at T')).toBeInTheDocument();
  });

  it('P2-04 术语渐进披露：技术字段开关默认显示，可隐藏插件技术标识', async () => {
    renderPanel();
    const { evenId } = await setupTwoFiles();
    fireEvent.click(screen.getByRole('button', { name: 'cursor-120' }));
    await advance(600);

    const entriesGroup = groupById(evenId);
    expect(within(entriesGroup).getByTestId('kv-plugin-id')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('kv-toggle-technical'));
    expect(within(entriesGroup).queryByTestId('kv-plugin-id')).not.toBeInTheDocument();
    expect(within(entriesGroup).getByText('field_1')).toBeInTheDocument();
    expect(within(entriesGroup).getByText(/entries/)).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('kv-toggle-technical'));
    expect(within(entriesGroup).getByTestId('kv-plugin-id')).toBeInTheDocument();
  });
});

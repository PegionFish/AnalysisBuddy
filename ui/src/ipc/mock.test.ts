import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { EV_PLUGIN_HEALTH, EV_PLUGIN_LOG, EV_PLUGINS_RELOADED, EV_PROGRESS } from './events';
import { createMockIpc } from './mock';
import type { PluginHealthPayload, PluginLogPayload, ProgressPayload } from './events';

/** Resolve a promise whose rejection/settling is driven by fake timers inside the mock. */
async function settle<T>(p: Promise<T>, ms = 300): Promise<T> {
  let done = false;
  let value: T | undefined;
  let failed: unknown;
  p.then(
    (v) => {
      done = true;
      value = v;
    },
    (e) => {
      done = true;
      failed = e;
    },
  );
  await vi.advanceTimersByTimeAsync(ms);
  expect(done).toBe(true);
  if (failed !== undefined) throw failed;
  return value as T;
}

/** Advance fake timers until the mock parse pipeline for a file completes (duration 3–8s + slack). */
async function finishParse(ms = 9_500): Promise<void> {
  await vi.advanceTimersByTimeAsync(ms);
}

describe('mock IPC (ipc-ui.md §3.3)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('list_plugins returns the two fake plugins as ready', async () => {
    const mock = createMockIpc();
    const plugins = await settle(mock.list_plugins());
    expect(plugins).toHaveLength(2);
    expect(plugins.map((p) => p.id).sort()).toEqual(['builtin-csv', 'demo-tool']);
    expect(plugins.every((p) => p.state === 'ready')).toBe(true);
  });

  it('imports a .csv file through the full health sequence to ready', async () => {
    const mock = createMockIpc();
    const health: PluginHealthPayload[] = [];
    const progress: ProgressPayload[] = [];
    mock.listen<PluginHealthPayload>(EV_PLUGIN_HEALTH, (h) => health.push(h));
    mock.listen<ProgressPayload>(EV_PROGRESS, (p) => progress.push(p));

    const p = mock.import_files({ paths: ['C:\\data\\run-2024.csv'] });
    const results = await settle(p, 400);
    expect(results).toHaveLength(1);
    expect(results[0].status).toBe('parsing');
    expect(results[0].matched_plugin?.plugin_id).toBe('builtin-csv');
    expect(results[0].matched_plugin?.confidence).toBeCloseTo(0.97);

    await finishParse();
    expect(progress.length).toBeGreaterThan(0);
    expect(progress[progress.length - 1].percent).toBe(100);
    expect(health.map((h) => h.state)).toEqual(
      expect.arrayContaining(['discovered', 'spawning', 'initializing', 'ready', 'parsing', 'ready']),
    );

    const tree = await settle(mock.get_metrics({}));
    expect(tree).toHaveLength(1);
    expect(tree[0].id).toBe(results[0].file_id);
    expect(tree[0].children?.[0].level).toBe('plugin');
    expect(tree[0].children?.[0].children?.length).toBeGreaterThanOrEqual(3);
  });

  it('is deterministic: same file_id and args produce identical query_series results (snapshot)', async () => {
    const mock = createMockIpc();
    const p = mock.import_files({ paths: ['C:\\data\\det.csv'] });
    const [result] = await settle(p);
    await finishParse();
    const args = {
      file_ids: [result.file_id],
      metrics: [`${result.file_id}:builtin-csv:metric-1`, `${result.file_id}:builtin-csv:metric-2`],
      t0_ms: 0,
      t1_ms: 600_000,
      max_points_per_series: 100,
    };
    const first = await settle(mock.query_series(args));
    const second = await settle(mock.query_series(args));
    expect(second).toEqual(first);
    expect(second).toMatchSnapshot();
    expect(second[0].downsampled).toBe(true);
    expect(second[0].points.length).toBeLessThanOrEqual(100);
  });

  it('key_values_at never rejects: -odd file gets a timeout entry, others get entries', async () => {
    const mock = createMockIpc();
    const p = mock.import_files({ paths: ['C:\\data\\kv.csv'] });
    const [result] = await settle(p);
    const fileId = result.file_id;
    expect(fileId.endsWith('-odd')).toBe(true);
    await finishParse();

    const kv = await settle(mock.key_values_at({ file_ids: [fileId, 'mock-unknown-1'], timestamp_ms: 1000 }));
    expect(kv).toHaveLength(2);
    expect(kv[0].error?.code).toBe('timeout');
    expect(kv[1].error?.code).toBe('plugin_busy');

    const p2 = mock.import_files({ paths: ['C:\\data\\even.csv'] });
    const [r2] = await settle(p2);
    expect(r2.file_id.endsWith('-odd')).toBe(false);
    await finishParse();
    const kv2 = await settle(mock.key_values_at({ file_ids: [r2.file_id], timestamp_ms: 1000 }));
    expect(kv2[0].error).toBeUndefined();
    expect((kv2[0].entries ?? []).length).toBeGreaterThanOrEqual(5);
  });

  it('injects failures: fail-load → file_load_failed, no-plugin → no_plugin_matched', async () => {
    const mock = createMockIpc();
    const results = await settle(mock.import_files({ paths: ['C:\\data\\fail-load.csv', 'C:\\data\\no-plugin.log'] }), 400);
    expect(results[0].status).toBe('error');
    expect(results[0].error?.code).toBe('file_load_failed');
    expect(results[1].status).toBe('error');
    expect(results[1].error?.code).toBe('no_plugin_matched');
  });

  it('needs_user_choice: choice path keeps matched_plugin null; override re-import proceeds to parsing', async () => {
    const mock = createMockIpc();
    const [first] = await settle(mock.import_files({ paths: ['C:\\data\\choice.dat'] }), 400);
    expect(first.status).toBe('matched');
    expect(first.needs_user_choice).toBe(true);
    expect(first.matched_plugin).toBeNull();
    expect(first.candidate_plugins).toHaveLength(2);
    expect(Math.abs(first.candidate_plugins[0].confidence - first.candidate_plugins[1].confidence)).toBeLessThan(0.1);

    const [second] = await settle(
      mock.import_files({ paths: ['C:\\data\\choice.dat'], overrides: { 'C:\\data\\choice.dat': { plugin_id: 'builtin-csv' } } }),
      400,
    );
    expect(second.file_id).toBe(first.file_id);
    expect(second.status).toBe('parsing');
    expect(second.matched_plugin?.plugin_id).toBe('builtin-csv');
  });

  it('emits plugin logs on startup and during parse', async () => {
    const mock = createMockIpc();
    const logs: { plugin_id: string; level: string }[] = [];
    mock.listen(EV_PLUGIN_LOG, (l) => logs.push(l as { plugin_id: string; level: string }));
    await settle(mock.import_files({ paths: ['C:\\data\\logs.csv'] }), 400);
    await vi.advanceTimersByTimeAsync(7_000);
    expect(logs.filter((l) => l.plugin_id === 'builtin-csv').length).toBeGreaterThanOrEqual(2);
    expect(logs.some((l) => l.level === 'warn')).toBe(true);
    const buf = await settle(mock.get_plugin_log({ plugin_id: 'builtin-csv' }));
    expect(buf.length).toBeGreaterThanOrEqual(2);
  });

  it('reload_plugin rebuilds the instance through a health cycle back to ready', async () => {
    const mock = createMockIpc();
    const health: PluginHealthPayload[] = [];
    mock.listen<PluginHealthPayload>(EV_PLUGIN_HEALTH, (h) => health.push(h));
    const logs: PluginLogPayload[] = [];
    mock.listen(EV_PLUGIN_LOG, (l) => logs.push(l as PluginLogPayload));

    const info = await settle(mock.reload_plugin({ plugin_id: 'builtin-csv' }));
    expect(info.state).toBe('ready');
    expect(health.map((h) => h.state)).toEqual(['spawning', 'initializing', 'ready']);
    expect(logs.some((l) => l.line.includes('reloaded'))).toBe(true);
  });

  it('reload_plugin rejects for unknown plugin ids', async () => {
    const mock = createMockIpc();
    await expect(settle(mock.reload_plugin({ plugin_id: 'ghost' }))).rejects.toMatchObject({ code: 'internal' });
  });

  it('unload_file is idempotent and removes files from get_metrics', async () => {
    const mock = createMockIpc();
    const [result] = await settle(mock.import_files({ paths: ['C:\\data\\unload.csv'] }));
    await finishParse();
    await settle(mock.unload_file({ file_id: result.file_id }));
    expect((await settle(mock.unload_file({ file_id: 'mock-unknown' })))).toBeUndefined();
    const tree = await settle(mock.get_metrics({}));
    expect(tree).toHaveLength(0);
  });

  it('save_session persists to localStorage and load_session replays files', async () => {
    const mock = createMockIpc();
    await settle(mock.import_files({ paths: ['C:\\data\\sess.csv'] }), 400);
    const meta = await settle(mock.save_session({ path: 'C:\\sessions\\s1.absession' }));
    expect(meta.path).toBe('C:\\sessions\\s1.absession');
    expect(localStorage.getItem('ab.mock.session')).toContain('s1.absession');

    const loaded = await settle(mock.load_session({ path: 'C:\\sessions\\s1.absession' }), 400);
    expect(loaded.loaded_file_ids).toHaveLength(1);
    expect(loaded.missing).toHaveLength(0);

    await expect(settle(mock.load_session({ path: 'C:\\sessions\\nope.absession' }), 400)).rejects.toMatchObject({
      code: 'file_not_found',
    });
  });

  it('load_session with "missing" in path marks stored files as missing', async () => {
    const mock = createMockIpc();
    await settle(mock.import_files({ paths: ['C:\\data\\gone.csv'] }), 400);
    await settle(mock.save_session({ path: 'C:\\sessions\\s2.absession' }), 400);
    const loaded = await settle(mock.load_session({ path: 'C:\\sessions\\missing.absession' }), 400);
    expect(loaded.loaded_file_ids).toHaveLength(0);
    expect(loaded.missing).toHaveLength(1);
    expect(loaded.missing[0].reason).toBe('not_found');
  });

  it('load_session with "reopen" in path reports reopen failures', async () => {
    const mock = createMockIpc();
    await settle(mock.import_files({ paths: ['C:\\data\\busy.csv'] }), 400);
    await settle(mock.save_session({ path: 'C:\\sessions\\s3.absession' }), 400);
    const loaded = await settle(mock.load_session({ path: 'C:\\sessions\\reopen.absession' }), 400);
    expect(loaded.loaded_file_ids).toHaveLength(0);
    expect(loaded.missing).toHaveLength(0);
    expect(loaded.reopen_failed).toHaveLength(1);
    expect(loaded.reopen_failed?.[0].path).toBe('C:\\data\\busy.csv');
    expect(loaded.reopen_failed?.[0].reason).toBe('reopen_failed');
  });
});

describe('mock IPC module manager (spec §4.1, task 7)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  function reloaded(): { mock: ReturnType<typeof createMockIpc>; events: unknown[] } {
    const mock = createMockIpc();
    const events: unknown[] = [];
    mock.listen(EV_PLUGINS_RELOADED, (e) => events.push(e));
    return { mock, events };
  }

  it('list_plugins reports fixture metadata: builtin flag, source and update_url', async () => {
    const { mock } = reloaded();
    const plugins = await settle(mock.list_plugins());
    const builtin = plugins.find((p) => p.id === 'builtin-csv');
    expect(builtin?.builtin).toBe(true);
    expect(builtin?.source).toBe('portable');
    expect(builtin?.update_url).toBeUndefined();
    const demo = plugins.find((p) => p.id === 'demo-tool');
    expect(demo?.builtin).toBe(false);
    expect(demo?.source).toBe('portable');
  });

  it('install_plugin_zip adds the fixture-csv plugin to the list and emits PluginsReloaded', async () => {
    const { mock, events } = reloaded();
    const info = await settle(mock.install_plugin_zip({ path: 'C:\\zips\\fixture.zip', overwrite: false }));
    expect(info.id).toBe('fixture-csv');
    expect(info.version).toBe('1.1.0');
    expect(info.builtin).toBe(false);
    expect(info.update_url).toBe('https://github.com/fixture/fixture-csv');
    expect(info.changelog).toHaveLength(3);

    const plugins = await settle(mock.list_plugins());
    expect(plugins.map((p) => p.id)).toEqual(['builtin-csv', 'demo-tool', 'fixture-csv']);
    expect(events.length).toBeGreaterThanOrEqual(1);
  });

  it('install_plugin_zip rejects bad zips, protected ids and conflicts without overwrite', async () => {
    const { mock } = reloaded();
    await expect(
      settle(mock.install_plugin_zip({ path: 'C:\\zips\\bad.zip', overwrite: false })),
    ).rejects.toMatchObject({ code: 'module_install' });
    await expect(
      settle(mock.install_plugin_zip({ path: 'C:\\zips\\protected.zip', overwrite: false })),
    ).rejects.toMatchObject({ code: 'module_protected' });
    await expect(
      settle(mock.install_plugin_zip({ path: 'C:\\zips\\conflict.zip', overwrite: false })),
    ).rejects.toMatchObject({ code: 'module_conflict', data: { version: '1.1.0' } });
  });

  it('install_plugin_zip with overwrite=true succeeds for conflict paths', async () => {
    const { mock } = reloaded();
    const info = await settle(mock.install_plugin_zip({ path: 'C:\\zips\\conflict.zip', overwrite: true }));
    expect(info.id).toBe('fixture-csv');
  });

  it('uninstall_plugin removes a plugin, rejects builtin ids and unknown ids', async () => {
    const { mock } = reloaded();
    await settle(mock.install_plugin_zip({ path: 'C:\\zips\\fixture.zip', overwrite: false }));
    await settle(mock.uninstall_plugin({ plugin_id: 'fixture-csv' }));
    const plugins = await settle(mock.list_plugins());
    expect(plugins.map((p) => p.id)).toEqual(['builtin-csv', 'demo-tool']);

    await expect(settle(mock.uninstall_plugin({ plugin_id: 'builtin-csv' }))).rejects.toMatchObject({
      code: 'module_protected',
    });
    await expect(settle(mock.uninstall_plugin({ plugin_id: 'ghost' }))).rejects.toMatchObject({
      code: 'module_not_found',
    });
  });

  it('set_plugin_enabled flips the disabled flag and unknown ids reject', async () => {
    const { mock, events } = reloaded();
    await settle(mock.set_plugin_enabled({ plugin_id: 'demo-tool', enabled: false }));
    const plugins = await settle(mock.list_plugins());
    expect(plugins.find((p) => p.id === 'demo-tool')?.disabled).toBe(true);
    expect(events.length).toBeGreaterThanOrEqual(1);

    await settle(mock.set_plugin_enabled({ plugin_id: 'demo-tool', enabled: true }));
    const reenabled = await settle(mock.list_plugins());
    expect(reenabled.find((p) => p.id === 'demo-tool')?.disabled).toBe(false);

    await expect(settle(mock.set_plugin_enabled({ plugin_id: 'ghost', enabled: false }))).rejects.toMatchObject({
      code: 'module_not_found',
    });
  });

  it('check_plugin_update: fixture-csv is newer, no-update_url plugins reject', async () => {
    const { mock } = reloaded();
    await settle(mock.install_plugin_zip({ path: 'C:\\zips\\fixture.zip', overwrite: false }));

    const info = await settle(mock.check_plugin_update({ plugin_id: 'fixture-csv' }));
    expect(info).toMatchObject({
      plugin_id: 'fixture-csv',
      current_version: '1.1.0',
      latest_version: '1.2.0',
      is_newer: true,
      asset_name: 'fixture-csv-v1.2.0.zip',
    });

    await expect(settle(mock.check_plugin_update({ plugin_id: 'builtin-csv' }))).rejects.toMatchObject({
      code: 'update_not_available',
    });
    await expect(settle(mock.check_plugin_update({ plugin_id: 'ghost' }))).rejects.toMatchObject({
      code: 'module_not_found',
    });
  });

  it('update_plugin bumps fixture-csv to 2.0.0, persists and emits PluginsReloaded', async () => {
    const { mock, events } = reloaded();
    await settle(mock.install_plugin_zip({ path: 'C:\\zips\\fixture.zip', overwrite: false }));

    const updated = await settle(mock.update_plugin({ plugin_id: 'fixture-csv' }));
    expect(updated.version).toBe('2.0.0');

    const plugins = await settle(mock.list_plugins());
    expect(plugins.find((p) => p.id === 'fixture-csv')?.version).toBe('2.0.0');
    expect(events.length).toBeGreaterThanOrEqual(1);

    await expect(settle(mock.update_plugin({ plugin_id: 'builtin-csv' }))).rejects.toMatchObject({
      code: 'update_not_available',
    });
  });
});

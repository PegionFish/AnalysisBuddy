/** ui/src/components/modules/knownModules.test.ts — 已知领域模块指纹目录（P1-01 建议 3）。
 *  指纹：文件名/路径关键字（忽略大小写）+ 内容列特征；recommendModuleForFile 返回推荐或 null。 */

import { describe, expect, it } from 'vitest';
import type { ImportResult } from '../../ipc/types';
import { recommendModuleForFile } from './knownModules';

function result(name: string): ImportResult {
  return {
    file_id: 'f-1',
    path: `C:\\data\\${name}`,
    name,
    size_bytes: 2_621_440,
    status: 'ready',
    matched_plugin: null,
    candidate_plugins: [],
  };
}

describe('recommendModuleForFile 领域模块指纹', () => {
  it('文件名含 hwinfo（忽略大小写）→ hwinfo-log 推荐', () => {
    const rec = recommendModuleForFile(result('ref_hwinfo.CSV'));
    expect(rec).not.toBeNull();
    expect(rec!.moduleId).toBe('hwinfo-log');
    expect(rec!.displayName).toBeTruthy();
    expect(rec!.adds.length).toBeGreaterThan(0);
  });

  it('文件名含 battery → batteryinfoview 推荐', () => {
    const rec = recommendModuleForFile(result('ref_batteryinfoview.txt'));
    expect(rec).not.toBeNull();
    expect(rec!.moduleId).toBe('batteryinfoview');
    expect(rec!.displayName).toBeTruthy();
    expect(rec!.adds.length).toBeGreaterThan(0);
  });

  it('内容指纹命中：SensorType/sensor 列特征与 BatteryInfoView/AC Power 关键字', () => {
    expect(recommendModuleForFile(result('export.csv'), 'Date Time,Value,SensorType')?.moduleId ?? null).toBe('hwinfo-log');
    expect(recommendModuleForFile(result('export.csv'), 'sensor temp,voltage')?.moduleId ?? null).toBe('hwinfo-log');
    expect(recommendModuleForFile(result('report.txt'), 'BatteryInfoView report')?.moduleId ?? null).toBe('batteryinfoview');
    expect(recommendModuleForFile(result('report.txt'), 'AC Power: AC')?.moduleId ?? null).toBe('batteryinfoview');
  });

  it('非命中样本 → null（无推荐）', () => {
    expect(recommendModuleForFile(result('normal.csv'))).toBeNull();
    expect(recommendModuleForFile(result('run-2026.log'), 'ordinary lines')).toBeNull();
  });
});

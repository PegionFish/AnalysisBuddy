/** ui/src/components/modules/knownModules.ts — 已知领域模块指纹目录（P1-01 建议 3）。
 *  仅前端静态目录（manifest 类数据）：文件名/路径关键字（忽略大小写）+ 内容列特征。
 *  内容特征需内容样本（contentHint，数据到达前以文件名先行命中）；
 *  displayName/adds 为目录数据，渲染策略与插件 manifest 数据（display_name 等）一致。 */

import type { ImportResult } from '../../ipc/types';

export interface ModuleRecommendation {
  moduleId: string;
  displayName: string;
  /** 安装该模块后新增的指标/上下文能力说明（目录数据，中文）。 */
  adds: string[];
}

export interface KnownModuleDef extends ModuleRecommendation {
  /** 文件名/路径指纹（忽略大小写）。 */
  namePatterns: RegExp[];
  /** 内容指纹（列特征/关键字；首个命中即推荐）。 */
  contentPatterns: RegExp[];
}

export const KNOWN_MODULES: KnownModuleDef[] = [
  {
    moduleId: 'hwinfo-log',
    displayName: 'HWiNFO 日志解析模块',
    adds: ['传感器指标（温度/电压/功耗/风扇转速等）', '按传感器分组的完整指标树', '传感器时间曲线与关键值'],
    namePatterns: [/hwinfo/i],
    contentPatterns: [/SensorType/i, /sensor/i],
  },
  {
    moduleId: 'batteryinfoview',
    displayName: 'BatteryInfoView 解析模块',
    adds: ['电池电量与容量指标', 'AC/DC 供电状态上下文', '充放电循环与电池健康度关键值'],
    namePatterns: [/battery/i],
    contentPatterns: [/BatteryInfoView/i, /AC Power/i],
  },
];

/** 按文件指纹推荐领域模块；未命中返回 null（调用方展示通用缺失提示）。
 *  contentHint 为可选内容样本（首行/前几行文本），文件名已命中时无需样本。 */
export function recommendModuleForFile(
  entry: ImportResult,
  contentHint?: string,
): ModuleRecommendation | null {
  const name = `${entry.name} ${entry.path}`;
  const content = contentHint ?? '';
  for (const def of KNOWN_MODULES) {
    const hit =
      def.namePatterns.some((re) => re.test(name)) ||
      def.contentPatterns.some((re) => re.test(content));
    if (hit) return { moduleId: def.moduleId, displayName: def.displayName, adds: [...def.adds] };
  }
  return null;
}

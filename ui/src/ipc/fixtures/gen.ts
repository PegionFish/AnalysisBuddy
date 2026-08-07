/** ui/src/ipc/fixtures/gen.ts — deterministic synthetic data generation (seeded LCG, ipc-ui.md §3.3).
 *  Any value derived from a seed string is reproducible: same file_id + same args → identical output. */

import type { KeyValueEntry, MetricNode, SeriesPoint, SeriesSlice } from '../types';

/** FNV-1a 32-bit hash of a string → unsigned int seed. */
export function hashSeed(input: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    h ^= input.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/** Deterministic LCG (numerical recipes constants), state advances per call. */
export class Lcg {
  private state: number;

  constructor(seed: number) {
    this.state = seed >>> 0;
    if (this.state === 0) this.state = 0x6d2b79f5;
  }

  /** Next float in [0, 1). */
  next(): number {
    this.state = (Math.imul(this.state, 1664525) + 1013904223) >>> 0;
    return this.state / 4294967296;
  }

  /** Next int in [0, max). */
  nextInt(max: number): number {
    return Math.floor(this.next() * max);
  }
}

/** Deterministic pseudo-random float in [-1, 1] for a given timestamp (no state, stable under slicing). */
function noiseAt(seed: string, tMs: number): number {
  const rng = new Lcg(hashSeed(`${seed}:${Math.round(tMs / 30)}`));
  return rng.next() * 2 - 1;
}

/** Base series constants: 10 minutes duration, ~20k points at 30ms interval. */
export const BASE_DURATION_MS = 600_000;
export const BASE_START_MS = 0;
const BASE_DT_MS = 30;
export const BASE_POINT_COUNT = BASE_DURATION_MS / BASE_DT_MS;

interface MetricDef {
  metric_id: string;
  name: string;
  unit?: string;
  description?: string;
  aggregation?: 'last' | 'sum' | 'avg' | 'min' | 'max';
  /** Amplitude of the primary sinusoid. */
  amplitude: number;
  /** Frequency (rad/ms) of the primary sinusoid. */
  frequency: number;
  /** DC offset. */
  offset: number;
}

/** Deterministic metric definitions for a file (3–5 metrics), stable per file_id. */
export function genMetricDefs(fileId: string): MetricDef[] {
  const seed = hashSeed(`metrics:${fileId}`);
  const rng = new Lcg(seed);
  const count = 3 + rng.nextInt(3);
  const defs: MetricDef[] = [];
  for (let i = 0; i < count; i++) {
    const amp = 10 + rng.next() * 90;
    const freq = (0.5 + rng.next() * 4) / 10_000;
    const offset = rng.next() * 100;
    const agg = (['last', 'sum', 'avg', 'min', 'max'] as const)[rng.nextInt(5)];
    defs.push({
      metric_id: `metric-${i + 1}`,
      name: `metric_${i + 1}`,
      unit: i % 3 === 0 ? 'ms' : i % 3 === 1 ? '%' : 'count',
      description: `synthetic metric ${i + 1} for ${fileId}`,
      aggregation: agg,
      amplitude: amp,
      frequency: freq,
      offset,
    });
  }
  return defs;
}

/** Value of a metric at a timestamp (two sinusoids + deterministic noise). */
export function valueAt(def: MetricDef, seed: string, tMs: number): number {
  const rng = new Lcg(hashSeed(`phase:${seed}:${def.metric_id}`));
  const p1 = rng.next() * Math.PI * 2;
  const p2 = rng.next() * Math.PI * 2;
  const a1 = def.amplitude;
  const a2 = def.amplitude * 0.3;
  const f2 = def.frequency * 3.1;
  return def.offset + a1 * Math.sin(def.frequency * tMs + p1) + a2 * Math.sin(f2 * tMs + p2) + noiseAt(seed, tMs);
}

/** Downsample by LTTB (largest-triangle-three-buckets). */
export function lttb(points: SeriesPoint[], threshold: number): SeriesPoint[] {
  const n = points.length;
  if (threshold >= n || threshold < 3) return points;
  const sampled: SeriesPoint[] = [];
  const bucketSize = (n - 2) / (threshold - 2);
  let a = 0;
  sampled.push(points[0]);
  for (let i = 0; i < threshold - 2; i++) {
    const rangeStart = Math.floor((i + 1) * bucketSize) + 1;
    const rangeEnd = Math.min(Math.floor((i + 2) * bucketSize) + 1, n - 1);
    const avgStart = Math.floor((i + 0) * bucketSize) + 1;
    const avgEnd = Math.min(Math.floor((i + 1) * bucketSize) + 1, n - 1);
    let avgX = 0;
    let avgY = 0;
    for (let j = avgStart; j < avgEnd; j++) {
      avgX += points[j].t_ms;
      avgY += points[j].v;
    }
    const avgN = Math.max(1, avgEnd - avgStart);
    avgX /= avgN;
    avgY /= avgN;
    const ax = points[a].t_ms;
    const ay = points[a].v;
    let maxArea = -1;
    let maxIdx = rangeStart;
    for (let j = rangeStart; j < rangeEnd; j++) {
      const area = Math.abs((ax - avgX) * (points[j].v - ay) - (ax - points[j].t_ms) * (avgY - ay));
      if (area > maxArea) {
        maxArea = area;
        maxIdx = j;
      }
    }
    sampled.push(points[maxIdx]);
    a = maxIdx;
  }
  sampled.push(points[n - 1]);
  return sampled;
}

/** Deterministic windowed series for one (file_id, plugin_id, metric) — slicing is stable under repeated calls. */
export function genSeries(
  fileId: string,
  pluginId: string,
  def: MetricDef,
  t0Ms: number,
  t1Ms: number,
  maxPoints: number,
): { points: SeriesPoint[]; downsampled: boolean } {
  const seed = `${fileId}:${pluginId}`;
  const t0 = Math.max(t0Ms, BASE_START_MS);
  const t1 = Math.min(t1Ms, BASE_START_MS + BASE_DURATION_MS);
  if (t1 <= t0) return { points: [], downsampled: false };
  const raw: SeriesPoint[] = [];
  for (let t = t0; t < t1; t += BASE_DT_MS) {
    raw.push({ t_ms: t, v: valueAt(def, seed, t) });
  }
  const downsampled = raw.length > maxPoints;
  return { points: downsampled ? lttb(raw, maxPoints) : raw, downsampled };
}

/** Deterministic per-file key-value entries (5–8 rows). */
export function genKeyValues(fileId: string): KeyValueEntry[] {
  const rng = new Lcg(hashSeed(`kv:${fileId}`));
  const count = 5 + rng.nextInt(4);
  const entries: KeyValueEntry[] = [];
  for (let i = 0; i < count; i++) {
    const kind = rng.nextInt(3);
    const key = `field_${i + 1}`;
    if (kind === 0) entries.push({ key, value: Math.round(rng.next() * 1000) / 10, unit: 'ms' });
    else if (kind === 1) entries.push({ key, value: Math.round(rng.next() * 1000) / 10, unit: '%' });
    else entries.push({ key, value: rng.next() > 0.5 });
  }
  return entries;
}

/** Build the three-level MetricNode tree for one ready file. */
export function genMetricTree(fileId: string, pluginId: string, pluginDisplayName: string): MetricNode {
  const defs = genMetricDefs(fileId);
  return {
    level: 'file',
    id: fileId,
    file_id: fileId,
    name: fileId,
    children: [
      {
        level: 'plugin',
        id: pluginId,
        file_id: fileId,
        plugin_id: pluginId,
        name: pluginDisplayName,
        children: defs.map((d) => ({
          level: 'metric',
          id: `${fileId}:${pluginId}:${d.metric_id}`,
          file_id: fileId,
          plugin_id: pluginId,
          metric_id: d.metric_id,
          name: d.name,
          unit: d.unit,
          description: d.description,
          aggregation: d.aggregation,
        })),
      },
    ],
  };
}

/** Helper to shape a SeriesSlice (keeps mock and tests independent from id parsing details). */
export function toSlice(
  fileId: string,
  pluginId: string,
  metricId: string,
  points: SeriesPoint[],
  downsampled: boolean,
): SeriesSlice {
  return { file_id: fileId, plugin_id: pluginId, metric_id: metricId, point_count: points.length, downsampled, points };
}

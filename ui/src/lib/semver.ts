/** ui/src/lib/semver.ts — minimal semver comparison for changelog sorting (spec §6.2).
 *  No dependency: compares x.y.z numerically, tolerates a v-prefix and partial versions. */

function parseParts(v: string): [number, number, number] {
  const m = /^v?(\d+)(?:\.(\d+))?(?:\.(\d+))?/.exec(v.trim());
  if (!m) return [0, 0, 0];
  return [Number(m[1] ?? 0), Number(m[2] ?? 0), Number(m[3] ?? 0)];
}

/** Numeric semver compare: a<b → <0, a>b → >0, equal → 0 (prerelease suffixes ignored). */
export function compareSemver(a: string, b: string): number {
  const pa = parseParts(a);
  const pb = parseParts(b);
  for (let i = 0; i < 3; i += 1) {
    if (pa[i] !== pb[i]) return pa[i] < pb[i] ? -1 : 1;
  }
  return 0;
}

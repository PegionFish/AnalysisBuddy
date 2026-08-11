/** ui/src/lib/semver.test.ts — minimal semver comparison used by changelog rendering (spec §6.2). */
import { describe, expect, it } from 'vitest';
import { compareSemver } from './semver';

describe('compareSemver (changelog sort, spec §6.2)', () => {
  it('orders numeric versions descending', () => {
    const sorted = ['1.0.5', '1.2.0', '1.1.0', '0.9.0'].sort(compareSemver);
    expect(sorted).toEqual(['0.9.0', '1.0.5', '1.1.0', '1.2.0']);
  });

  it('handles v-prefixed and partial versions', () => {
    expect(compareSemver('v1.2.0', '1.1.9')).toBeGreaterThan(0);
    expect(compareSemver('1.2', '1.2.0')).toBe(0);
    expect(compareSemver('2.0', '1.9.9')).toBeGreaterThan(0);
  });

  it('treats equal versions as equal', () => {
    expect(compareSemver('1.2.0', '1.2.0')).toBe(0);
    expect(compareSemver('v1.2.0', '1.2.0')).toBe(0);
  });
});

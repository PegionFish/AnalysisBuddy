/** scripts/check-i18n.mjs — CI key-tree consistency check between zh.json and en.json (ipc-ui.md §6).
 *  Exits 1 with the diff when the key sets differ. */

import { readFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const dir = dirname(fileURLToPath(import.meta.url));
const zh = JSON.parse(readFileSync(resolve(dir, '../src/i18n/zh.json'), 'utf8'));
const en = JSON.parse(readFileSync(resolve(dir, '../src/i18n/en.json'), 'utf8'));

function flattenKeys(obj, prefix = '') {
  const keys = new Set();
  for (const [key, value] of Object.entries(obj)) {
    const path = prefix ? `${prefix}.${key}` : key;
    if (value && typeof value === 'object') {
      for (const k of flattenKeys(value, path)) keys.add(k);
    } else {
      keys.add(path);
    }
  }
  return keys;
}

const zhKeys = flattenKeys(zh);
const enKeys = flattenKeys(en);
const onlyZh = [...zhKeys].filter((k) => !enKeys.has(k));
const onlyEn = [...enKeys].filter((k) => !zhKeys.has(k));

if (onlyZh.length > 0 || onlyEn.length > 0) {
  console.error('i18n key tree mismatch:');
  for (const k of onlyZh) console.error(`  only in zh.json: ${k}`);
  for (const k of onlyEn) console.error(`  only in en.json: ${k}`);
  process.exit(1);
}
console.log(`i18n key trees consistent: ${zhKeys.size} keys`);

#!/usr/bin/env bash
# E1（P0-3-1，总计划 §1.6）：builtin 交付一致性断言。
# 源真相 = 仓库 plugins/ 下含 plugin.json 的直接子目录（与 core/ab-engine/
# build.rs 扫描规则同源）——每一个都必须出现在交付目录中（含 plugin.json），
# 否则「已内建却不可用」（409 module_protected 死锁）在发布时就该失败。
# 用法：check-builtin-delivery.sh <交付的 plugins 目录>
set -euo pipefail
REPO_PLUGINS="$(cd "$(dirname "$0")/.." && pwd)/plugins"
DELIVERY="${1:?usage: check-builtin-delivery.sh <delivered plugins dir>}"

fail=0
for d in "$REPO_PLUGINS"/*/; do
  id=$(basename "$d")
  [ -f "$d/plugin.json" ] || continue
  if [ ! -f "$DELIVERY/$id/plugin.json" ]; then
    echo "MISSING: builtin plugin `$id` not found in delivery ($DELIVERY/$id/plugin.json)" >&2
    fail=1
  fi
done
if [ "$fail" -eq 0 ]; then
  echo "builtin delivery OK: $(find "$REPO_PLUGINS" -mindepth 2 -maxdepth 2 -name plugin.json | wc -l | tr -d ' ') builtin plugin(s) all delivered"
fi
exit "$fail"

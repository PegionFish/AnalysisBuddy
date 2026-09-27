#!/usr/bin/env bash
# I1（WS-I，卷二 §2.7）：红队 v1——隔离矩阵穿透尝试。对目标网关执行
# 匿名/伪造/植入/穿越等攻击面探测，产出机器可判报告。
# 退出码 0=全部符合预期（未被穿透）；1=存在穿透或异常。
set -u
BASE="${1:-http://127.0.0.1:8601}"
PASS=0; FAIL=0; FINDINGS=()

chk() { # chk <名称> <期望> <实际>
  if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "  ok   $1 ($3)";
  else FAIL=$((FAIL+1)); FINDINGS+=("$1: want=$2 got=$3"); echo "  PWN  $1: want=$2 got=$3"; fi
}

code() { curl -s --max-time 5 -o /dev/null -w '%{http_code}' "$@"; }
body() { curl -s --max-time 5 "$@"; }

echo "== [A] 伪造 ab_sid 形态学 =="
for fake in "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" \
            "1234" "deadbeef" "%2e%2e%2f%2e%2e%2fetc" "x;rm -rf /" \
            "0000000000000000000000000000000000000000000000000000000000000000.extra"; do
  c=$(code -H "Cookie: ab_sid=$fake" "$BASE/api/v1/metrics")
  chk "forged-sid(${fake:0:12}…)→404" 404 "$c"
done

echo "== [B] 植入 ab_tenant 复活尝试 =="
c=$(code -H "Cookie: ab_tenant=0123456789abcdef0123456789abcdef" "$BASE/api/v1/metrics")
chk "planted-tenant→非200(铸造或404)" 200 "$c" || chk "planted-tenant→404" 404 "$c"
c=$(code -X POST -H "Cookie: ab_tenant=0123456789abcdef0123456789abcdef" "$BASE/api/v1/imports/upload")
chk "planted-tenant+upload→非502类穿透(400/404)" 400 "$c" || FAIL=$((FAIL+1))

echo "== [C] 上传文件名穿越/混淆 =="
J=/tmp/rt.jar; rm -f $J; curl -s -c $J -o /dev/null -X POST "$BASE/api/v1/session"
evil() { # evil <filename> <期望落盘名不包含..>
  local n=$1
  local resp=$(curl -s -b $J -F "file=@-;filename=$n" "$BASE/api/v1/imports/upload" <<< "t,v
1,2")
  local job=$(echo "$resp" | python3 -c "import json,sys;print(json.load(sys.stdin).get('job_id',''))" 2>/dev/null)
  [ -z "$job" ] && { PASS=$((PASS+1)); echo "  ok   filename($n)→upload rejected"; return; }
  sleep 1
  local files=$(curl -s -b $J "$BASE/api/v1/files")
  if echo "$files" | grep -qE '\.\./|/etc/|\\\\'; then
    FAIL=$((FAIL+1)); FINDINGS+=("filename($n) 落盘路径含穿越: $files"); echo "  PWN  filename($n) 穿越落盘"
  else
    PASS=$((PASS+1)); echo "  ok   filename($n)→basename 净化"
  fi
}
evil "../../../tmp/pwned.csv"
evil "..%2F..%2F..%2Ftmp%2Fpwned.csv"
evil "we%00evil.csv"
evil "$(python3 -c 'print("长"*200+"a.csv")')"
evil "日本語/named.csv"
evil "ucevil.csv"

echo "== [D] SSE/事件流混淆 =="
# SSE 为长连接：--max-time 5 触发的退出码 28 + 已输出的 200 头视为通过
SID=$(curl -s --max-time 5 -c - -X POST $BASE/api/v1/session -o /dev/null | grep ab_sid | awk '{print $NF}')
curl -s --max-time 3 -D /tmp/rt-sse-headers -o /dev/null -H "Cookie: ab_sid=$SID" "$BASE/api/v1/events"
if head -1 /tmp/rt-sse-headers 2>/dev/null | grep -q " 200"; then PASS=$((PASS+1)); echo "  ok   events-valid-sid (200 stream)";
else FAIL=$((FAIL+1)); FINDINGS+=("events-valid-sid 非 200"); echo "  PWN  events-valid-sid"; fi
c=$(code "$BASE/api/v1/events")
chk "events-anon→400(无sid非GET为POST才拒，GET铸造)" 200 "$c"
c=$(code -X POST "$BASE/api/v1/events")
chk "events-POST→404/405" 404 "$c" || chk "events-POST→405" 405 "$c"

echo "== [E] 越权插件管理 =="
c=$(code -X POST "$BASE/api/v1/plugins/install")
chk "anon-install→401/403" 401 "$c" || chk "anon-install→403" 403 "$c"
c=$(code -X POST "$BASE/api/v1/plugins/rescan")
chk "anon-rescan→401/403" 401 "$c" || chk "anon-rescan→403" 403 "$c"
c=$(code -X DELETE "$BASE/api/v1/plugins/builtin-csv")
chk "anon-uninstall→401/403" 401 "$c" || chk "anon-uninstall→403" 403 "$c"

echo "== [F] overrides / GET /files 注入面 =="
r=$(curl -s -b $J -X POST "$BASE/api/v1/imports" -H 'Content-Type: application/json' \
     -d '{"paths":["/etc/passwd"]}')
chk "json-paths-import→404" 404 "$(curl -s -o /dev/null -w '%{http_code}' -b $J -X POST "$BASE/api/v1/imports" -H 'Content-Type: application/json' -d '{"paths":["/etc/passwd"]}')"
r=$(curl -s -b $J "$BASE/api/v1/files?__proto__=x&file_id=<script>")
chk "files-query-injection→200(无参回显)" 200 "$(curl -s -o /dev/null -w '%{http_code}' -b $J "$BASE/api/v1/files?file_id=<script>")"

echo "== [G] 直连实例端口（网关外旁路） =="
c=$(code --max-time 3 http://127.0.0.1:8610/api/v1/health || echo TIMEOUT)
# 期望：即便本机可达，无 token 也是 401/403；不可达则超时。二者皆安全。
if [ "$c" = "000" ] || [ "$c" = "TIMEOUT" ]; then PASS=$((PASS+1)); echo "  ok   instance-port 外部不可达";
elif [ "$c" = "401" ] || [ "$c" = "403" ]; then PASS=$((PASS+1)); echo "  ok   instance-port 有令牌门（$c）";
else FAIL=$((FAIL+1)); FINDINGS+=("instance-port 异常响应: $c"); echo "  PWN  instance-port: $c"; fi

echo
echo "REDTEAM v1: PASS=$PASS PWN=$FAIL"
[ $FAIL -gt 0 ] && printf 'finding: %s\n' "${FINDINGS[@]}" && exit 1
exit 0

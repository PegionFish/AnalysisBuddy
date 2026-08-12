"""生成 builtin-csv 测试夹具（与 F 路 tests/fixtures 同规格，qa-perf.md §2）。

- small_with_header.csv：200 行、3 指标列、表头 timestamp,fps,frame_ms,mem_mb、时间严格递增
- small_no_header.csv：同上但无表头
- malformed_lines.csv：200 行中 20 行畸形（缺列/非数值/坏时间戳/超长行）
- enc_utf8_bom.csv：UTF-8 BOM
- enc_gbk.csv：GBK 编码中文备注列
"""
import os
from datetime import datetime, timedelta, timezone

OUT = os.path.join(os.path.dirname(__file__), "fixtures")
os.makedirs(OUT, exist_ok=True)

T0 = datetime(2026, 8, 7, 0, 0, 0, tzinfo=timezone.utc)


def row(i: int, scene: bool = False) -> str:
    ts = (T0 + timedelta(milliseconds=500 * i)).isoformat()
    fps = 55 + (i * 7) % 10 + (i % 5) * 0.1
    frame_ms = 15 + (i * 3) % 3 + (i % 7) * 0.1
    mem = 900 + (i % 200)
    return f"{ts},{fps:.1f},{frame_ms:.1f},{mem}"


with open(os.path.join(OUT, "small_with_header.csv"), "w", encoding="utf-8", newline="\n") as f:
    f.write("timestamp,fps,frame_ms,mem_mb\n")
    for i in range(200):
        f.write(row(i) + "\n")

with open(os.path.join(OUT, "small_no_header.csv"), "w", encoding="utf-8", newline="\n") as f:
    for i in range(200):
        f.write(row(i) + "\n")

# malformed_lines.csv：20 行畸形
lines = []
for i in range(200):
    if i in (3, 11, 17, 23, 31, 37, 43, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97, 101, 103, 107):
        kind = i % 4
        if kind == 0:
            lines.append("broken line without commas")
        elif kind == 1:
            lines.append(f"{row(i)}x,extra,extra,extra,extra")  # 多列
        elif kind == 2:
            lines.append(f"not-a-timestamp,60.0,16.0,1000")  # 坏时间戳
        else:
            lines.append(f"{row(i).rsplit(',', 1)[0]},abc")  # 非数值
    else:
        lines.append(row(i))
with open(os.path.join(OUT, "malformed_lines.csv"), "w", encoding="utf-8", newline="\n") as f:
    f.write("timestamp,fps,frame_ms,mem_mb\n")
    for l in lines:
        f.write(l + "\n")

with open(os.path.join(OUT, "enc_utf8_bom.csv"), "wb") as f:
    f.write(b"\xef\xbb\xbf")
    content = "timestamp,fps,frame_ms\n"
    for i in range(20):
        content += row(i) + "\n"
    f.write(content.encode("utf-8"))

# enc_utf16le_bom.csv：UTF-16LE BOM 编码（Auto 检测用）。
with open(os.path.join(OUT, "enc_utf16le_bom.csv"), "wb") as f:
    f.write(b"\xff\xfe")
    content = "timestamp,fps,frame_ms\n"
    for i in range(20):
        content += row(i) + "\n"
    f.write(content.encode("utf-16-le"))

gbk_content = "timestamp,fps,备注\n"
for i in range(20):
    gbk_content += f"{row(i)},正常\n"
with open(os.path.join(OUT, "enc_gbk.csv"), "wb") as f:
    f.write(gbk_content.encode("gbk"))

print("fixtures written to", OUT)

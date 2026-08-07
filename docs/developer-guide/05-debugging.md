# 05 · 排错手册（症状 → 规则 ID → 根因 → 修复动作）

> 本表与 `plugin check` 的 21 条规则 ID 一一对应（规则清单见
> `AnalysisBuddy-devdocs/deep-dive/docs-validator.md` §2）。规则 ID 一经发布即冻结，
> 新增规则只能追加编号；**任何新增规则必须在同批提交内补对应的排错条目**。
>
> 使用方式：按现象在「症状」列定位 → 得到规则 ID → 看「典型根因」确认 → 执行
> 「修复动作」→ 重跑 `plugin check` 验证。

## 结构类（MAN-xx）

| 症状 | 规则 ID | 典型根因 | 修复动作 |
|------|---------|----------|----------|
| 插件页显示「清单校验失败」；`plugin check` 报 MAN-01 | `MAN-01` | `plugin.json` 缺必填字段（`id`/`display_name`/`version`/`entry`/`match`/`min_protocol_version` 任一）或类型错误（如 `version` 写成数字） | 对照 `docs/spec/plugin-manifest.schema.json` 的 `required` 与类型逐字段检查，补全或改正 |
| 拖入 `plugins/` 后插件列表显示 id 冲突/不匹配 | `MAN-02` | `id` 与插件目录名不一致；或目录树内存在两个相同 `id` 的清单 | 把目录名改成与 `id` 完全一致；确保 id 全局唯一 |
| 进程一直处于「启动失败」；日志无任何输出 | `MAN-03` | `entry.command` 相对路径下文件不存在（如 `target/release/xx.exe` 还没构建）；`entry.working_dir` 指向不存在的目录 | 先执行标准构建命令产出入口文件；确认 `command`/`working_dir` 相对 `plugin.json` 所在目录的路径正确 |
| `plugin check` 输出 MAN-04 警告 | `MAN-04` | 从别处复制的 manifest 里 `entry` 带盘符/UNC 绝对路径 | 改为相对 `plugin.json` 所在目录的路径（保留「仓库拖入即用」的可移植性） |
| 插件页提示「需要更高版本的宿主」 | `MAN-05` | `min_protocol_version` 大于宿主支持版本（如误填 `2`） | 改成宿主支持的版本（当前宿主版本以协议正本为准）；确需更高协议则升级宿主 |
| 插件永远不被自动发现，只能手动选文件 | `MAN-06` | `match.extensions` 与 `header_fingerprints` 同时为空 | `match` 至少给 `extensions` 或 `header_fingerprints` 之一 |
| `plugin check` 输出 MAN-07 警告 | `MAN-07` | `version` 不是语义化版本号（如 `"v1.0"`、`"1.0"`） | 写成 `主.次.修` 形态（如 `"0.1.0"`），格式见 protocol-v1.md §7.2 |
| 拖入 `plugins/` 后不显示 | `MAN-08` | `plugin.json` 在子目录（如 `src/`）；或目录里复制出了多个 `plugin.json` | 把唯一的 `plugin.json` 移到插件文件夹根部，删除多余副本 |
| （无告警场景——反向验收项） | `MAN-09` | —— | 目录内存在 `.git/`、源码、构建中间产物属正常，不得产生任何告警；若报了警告，检查是否误把无关文件当作插件内容处理 |

## 行为类（BEH-xx）

| 症状 | 规则 ID | 典型根因 | 修复动作 |
|------|---------|----------|----------|
| 宿主插件页显示「握手失败」 | `BEH-01` | `initialize` 响应漏字段（`id`/`name`/`version`/`capabilities` 任一）；返回的 `id` 与 manifest 不一致；`can_handle` 的置信度越界 | 补齐元数据四字段；`id` 必须与 manifest 一致；置信度保持在闭区间 `[0, 1]` |
| 请求发出后响应错配或无响应 | `BEH-02` | 响应 `id` 与请求不匹配（插件自造/复用 id） | 响应 `id` 必须逐字回显请求 `id`，插件不得发明 id |
| 插件页报「方法未实现」 | `BEH-03` | 对必选方法（initialize/schema/can_handle/load_file/parse/key_values/unload_file/shutdown）返回 `-32601`；或使用协议外的自定义错误码 | 必选方法给最小可用实现；错误码只用标准集与 `-32001`~`-32005` |
| 解析中途 UI 报「插件无响应」/ Timeout | `BEH-04` | 长任务循环内忘了发心跳；相邻进度通知间隔超过协议上限 | parse 期间周期性发送 `progress` 或 `RecordBatch`（无新数据也发，`records_so_far` 保持上次值；间隔以协议正本为准，见 protocol-v1.md §3.3） |
| 个别记录不上图；插件日志面板出现「丢弃」计数 | `BEH-05` | `Record.metric` 拼写与 `schema()` 声明不一致；`Record` 缺 `timestamp`/`metric`/`value`；可选字段输出 `null` 或空容器；数值含 NaN/Infinity | 校正 metric 拼写；补齐三必填；可选字段为空时整体省略键；过滤非有限数值 |
| 解析结束但条数对不上；宿主终止会话 | `BEH-06` | `RecordBatch.seq` 缺号或重复；`records_total` ≠ 各批 `records.length` 之和 | `seq` 从 0 严格递增不跳号；最终响应数值 = 实际发送记录总数 |
| key_values 面板显示「无数据」或「超时」 | `BEH-07` | 响应结构不符 `KeyValuesResult`（如 `entries` 不是数组、value 是对象）；或响应超时 | 返回 `{"entries": [{"key", "value", "unit"?}]}` 形状；value 限 string/number/boolean；保证在协议时限内响应 |
| 宿主报协议错误（`-32700` 行超限）并终止会话 | `BEH-08` | 单行消息字节数超过协议上限（如单批记录过多、`raw_line` 过长） | 缩小每批记录数；`raw_line` 抽样保留；行上限以协议正本为准（见 protocol-v1.md §1.3） |
| 逻辑正确却握手/解析异常 | `BEH-09` | stdout 混入调试打印、BOM、`\r\n` 行尾或孤立 `\r` | 一切日志走 stderr；stdout 只输出协议帧（UTF-8 无 BOM、LF 行尾） |
| 关闭会话后进程残留 | `BEH-10` | 收到 `shutdown` 后没有退出 | `shutdown` 应答后立即 flush 并退出（退出码 0） |
| 二次加载同一文件失败 | `BEH-11` | `load_file` 非幂等：同一 `file_id` 二次加载报错 | `load_file` 幂等重入（等价于先 unload 再 load，见 protocol-v1.md §9 第 2 条） |
| 宿主退出后留下孤儿进程 | `BEH-12` | stdin EOF 后插件不自行退出 | 实现 stdin EOF → 自行退出（退出码 0），见 protocol-v1.md §9 第 5 条 |

## 退出码速查（`plugin check`）

| 退出码 | 含义 | 下一步 |
|--------|------|--------|
| `0` | 通过 | —— |
| `1` | 仅警告 | 按 MAN-04/06/07、BEH-10/11/12 处理 |
| `2` | 存在 error | 按上表对应规则修复 |
| `3` | 用法错误 | 检查目录路径与参数拼写 |
| `4` | 校验器自身故障 | 检查 `--schema-dir` 指向的 Schema 是否齐全 |

---

📌 章节要点（双视角）

👤 **给人**：排错顺序固定为「插件页健康状态 → stderr 最后一屏 → 本表按症状定位
规则 ID」；进程崩溃十有八九在 stderr 尾部堆栈里，不要先去翻代码。

🤖 **给 Agent**：收到 `plugin check --json` 的 `rules` 数组后，按 `rules[].id`
在本表定位并执行「修复动作」，重跑直至 `exit_code == 0`；若某规则 ID 在本表
找不到对应行，说明该规则是新追加编号，按「新增规则必须同批补排错条目」纪律
上报，而不是自行猜测修复。

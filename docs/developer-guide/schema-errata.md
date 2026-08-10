# 契约实测勘误报告（schema-errata）

> 依据：E-03 实测（validator 结构/行为两阶段 × 复现用例集），契约基准 =
> `docs/spec/plugin-manifest.schema.json` + `docs/spec/rpc-messages.schema.json` +
> `docs/spec/protocol-v1.md`（contract-v1 冻结）。
>
> 冻结纪律：docs/spec 属契约，**只读不写**；判为 Schema/契约缺陷的条目必须以
> `contract-change-proposal-template.md` 提变更提案，走「主代理审批 + 全路广播」
> （PLAN.md §6 规则 1）；未获批前零改动。
>
> 判定四分类：**Schema 缺陷**（false reject / false accept）/ **实现缺陷**（插件
> 与契约不符）/ **非缺陷**（设计如此）/ **文档缺陷**（文档间表述不一致，归 E-01
> 指南修订，不触发契约评审）。

## 勘误条目

### E-01 · key_values 响应深层形状缺失 → Schema 缺陷（false accept）

| 列 | 内容 |
|----|------|
| 现象 | `{"jsonrpc":"2.0","id":7,"result":{"entries":"nope"}}` 通过 rpc-messages Schema，但违反 protocol-v1.md §2.6 `KeyValuesResult`（entries 必须为数组） |
| 复现帧 | `tests/schema_feedback/frames/kv-entries-non-array.json`；validator 实测：`schema_feedback_repro_cases_match_expectations`（期望翻转 = 修订信号） |
| 涉及 Schema 与 JSON Pointer | `rpc-messages.schema.json` → `#/definitions/SuccessResponse/properties/result`（仅 `{"type":"object"}`，无 per-method 深层形状） |
| 判定 | **Schema 缺陷**（false accept：SDK 合规输出不受影响，但明显违规帧未被 Schema 捕获；validator 以 BEH-07 规则层补偿） |
| 处置 | 提案 A（见下）提交主代理审批；获批后修订 rpc-messages.schema.json（新增 `#/definitions/KeyValuesResult` 等 result 形状定义）并同批同步 validator 规则引用 |

### E-02 · initialize 响应元数据形状缺失 → Schema 缺陷（false accept）

| 列 | 内容 |
|----|------|
| 现象 | `{"jsonrpc":"2.0","id":1,"result":{"id":"x"}}` 通过 rpc-messages Schema，但违反 protocol-v1.md §2.1 `InitializeResult`（id/name/version/capabilities 四必选） |
| 复现帧 | `tests/schema_feedback/frames/init-result-missing-fields.json` |
| 涉及 Schema 与 JSON Pointer | `rpc-messages.schema.json` → `#/definitions/SuccessResponse/properties/result`（generic object） |
| 判定 | **Schema 缺陷**（false accept；validator 以 BEH-01 规则层补偿） |
| 处置 | 与 E-01 同根因，并入提案 A |

### E-03 · 响应帧无 method 字段 → per-method 判别不可行（非缺陷，记录观察）

| 列 | 内容 |
|----|------|
| 现象 | `SuccessResponse`/`ErrorResponse` 不带 `method`（契约 §1.4 设计如此），Schema 无法按方法判别 result 形状——E-01/E-02 的根因 |
| 复现帧 | 无（设计特征，非违规帧） |
| 涉及 Schema 与 JSON Pointer | `rpc-messages.schema.json` → `#/definitions/SuccessResponse` |
| 判定 | **非缺陷**（协议正本 §1.4 响应由 id 关联、不带 method；为响应引入 method 将破坏契约）。深层校验由 validator 规则承担（BEH-01/07 等），docs-validator.md §3.2「Schema 失败折算」仅覆盖帧级结构 |
| 处置 | 记录观察；提案 A 若获批，以「id → 在途方法」的语义判别在 validator 层引用 per-method 定义，Schema 本身仍保持 oneOf 帧级判别 |

### E-04 · MAN-07 与 Schema version pattern 关系未说明（文档缺陷）

| 列 | 内容 |
|----|------|
| 现象 | plugin-manifest.schema.json 的 `version` pattern 为严格 semver；docs-validator.md §2.1 声称 MAN-07「version 非 semver → warning（宽松解析）」——实测 `"version":"1.0"` 同时触发 MAN-01（Schema 严格 pattern）与 MAN-07（宽松层），双重报告，且文档未说明两层关系 |
| 复现帧 | `tools/plugin-validator/tests/fixtures/bad-man-07-version/`（实测输出含 MAN-01 + MAN-07） |
| 涉及 Schema 与 JSON Pointer | `plugin-manifest.schema.json` → `#/properties/version/pattern` |
| 判定 | **文档缺陷**（docs-validator.md §2.1 与 Schema 表述不一致；不触发契约评审） |
| 处置 | E-01 指南已按「MAN-07 = Schema 放宽时的漂移守护层」说明（见 `docs/developer-guide/04-manifest-reference.md` 与 validator `structure.rs` MAN-07 注释）；建议 docs-validator.md §2.1 补充同义说明 |

### E-05 · error 级规则数量 14 ≠ 文档声称 15（文档缺陷）

| 列 | 内容 |
|----|------|
| 现象 | docs-validator.md §2.2/附录声称「error 级 15 条」；逐条核对为 14 条（MAN-01/02/03/05/08 共 5 + BEH-01~09 共 9） |
| 复现帧 | 无（静态核对；`rules.rs::rule_ids_frozen_and_sorted` 固化 21 条与级别裁定） |
| 判定 | **文档缺陷**（计数笔误；不触发契约评审） |
| 处置 | 已在本报告记录；`docs/developer-guide/05-debugging.md` 覆盖全部 error 级规则（14 条 + 全部 warning 级），待 docs-validator.md 修订 |

### E-06 · 正例/反例控制（非缺陷，全过）

| 现象 | 判定 | 处置 |
|------|------|------|
| docs/spec/examples 全部 15 帧复核：`frame-ok-*`/`manifest-ok` 全过、`frame-bad-*` 全拒（`schema_feedback_docs_examples_verified` 常备） | 非缺陷 | 契约正本 §3.5「机器可校验副本」承诺成立 |
| percent 越界、Record 可选字段 null、错误码越界、result+error 并存、id 非整数 → 均被 Schema 拒绝 | 非缺陷（正判） | 常备回归锁定 |
| manifest 顶层附加字段（author/license）→ 通过 | 非缺陷（additionalProperties 设计） | 常备回归锁定 |
| seq 重复/缺号、confidence 越界 → Schema 通过但 validator 规则（BEH-06/BEH-01）拦截 | 非缺陷（规则层覆盖） | 常备回归锁定 |
| 两份 Schema 与 ab-protocol 类型一致（skip-if-empty、错误码集合、id pattern 等逐项比对） | 非缺陷 | 无 |

### E-07 · progress 示例帧 percent 误用 0-1 量纲 → 文档缺陷（已修复）

| 列 | 内容 |
|----|------|
| 现象 | 契约正本 §3.3 与 `rpc-messages.schema.json`（`#/definitions/ProgressNotification/params/percent`，minimum 0 / maximum 100）均定义 percent ∈ [0, 100]；但 §3.5 示例③、`docs/spec/examples/frame-ok-06-progress.json`（percent 0.8）与 `frame-bad-structure.json`（percent 0.5）误用 0-1 量纲，会误导插件作者上报 0-1 分数 |
| 复现帧 | `docs/spec/examples/frame-ok-06-progress.json`（修正前 `"percent":0.8`；0.8 落在 [0,100] 内，Schema 不会拒绝，属静默误导而非 false accept/reject） |
| 涉及 Schema 与 JSON Pointer | `rpc-messages.schema.json` → `#/definitions/ProgressNotification/properties/params/properties/percent`（Schema 本身正确，未改动） |
| 判定 | **文档缺陷**（示例帧与契约量纲不一致；不触发契约评审） |
| 处置 | 已修复：§3.5 示例③与两份 example 帧的 percent 改为 [0,100] 量纲（0.8→80.5、0.5→50；取 80.5 而非 80 是为保持 `serde_tests.rs` 逐字往返断言成立——整值经 f64 重序列化后 `Number` 表示由整数变浮点，语义相等比较会失败）；逐字引用同步修订（`core/ab-protocol/src/serde_tests.rs`、`sdk/dotnet/tests/SerializationTests.cs`）；`schema_feedback_docs_examples_verified` 全量复核仍 frame-ok-* 全过、frame-bad-* 全拒（bad 帧拒因 = 通知携带 id，与 percent 无关）。另顺带修订 `core/ab-protocol/src/types.rs` `KeyValueEntry.value` 注释：原称「schema 层限制 string/number/boolean」与事实不符（schema 对 result 无逐方法深层形状，见 E-01），改为「契约约定标量、无运行时形状校验、插件自律」 |

## 实现缺陷（如有）

D1/D2 插件未合入（依赖状态见下），暂无「插件实现与契约不符」的实测对象；该分类
条目将在四个插件合入后补充，并转 issue 给对应路（D1/D2）。

## 依赖状态（D1/D2 交叉项）

| 实测对象 | 卡片 | 状态 | 复验动作 |
|----------|------|------|----------|
| sample-plugin（Python SDK 样例） | D1-02 | 未合入（并行开发中） | `plugin check <dir> --behavior --fixture <样例>` 退出码 0；差异入本表 |
| builtin-csv | D1-03 | 未合入 | 同上 |
| sample-plugin-csharp（C# SDK 样例） | D2-02 | 未合入 | 同上 |
| demo-tool | D2-03 | 未合入 | `plugin check ..\..\plugins\demo-tool --behavior --json > tests\schema_feedback\demo-tool.json`（产物落档本目录） |
| 宿主对四插件门禁 = 退出码 0 | D1-02/D1-03/D2-02/D2-03 | 未合入 | 随卡片就绪逐项勾掉（E-02 DoD 跨路联调项） |

**本文件标注：以上复验项全部「待 D1-02/D2-02 合入后复验」；当前不阻塞 E-03
交付（复现用例集与勘误机制已就绪并常备回归）。**

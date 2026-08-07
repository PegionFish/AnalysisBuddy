# tests/schema_feedback/ —— Schema 复现用例集（E-03）

本目录承载 E-03 契约实测反哺的**复现用例集**：触发 Schema 误判/漏判的最小帧与
期望判定。帧文件同时被 `src/schema_feedback.rs` 的常备回归单测读取（`cargo test
schema_feedback`），契约修订后自动重放。

## 用例清单

| 帧文件 | 涉及 Schema | 期望（Schema 判定） | 判定分类（见 docs/developer-guide/schema-errata.md） |
|--------|-------------|---------------------|------------------------------------------------------|
| `frames/kv-entries-non-array.json` | rpc-messages | **通过**（false accept） | Schema 缺陷（errata E-01） |
| `frames/init-result-missing-fields.json` | rpc-messages | **通过**（false accept） | Schema 缺陷（errata E-02） |
| `frames/record-batch-duplicate-seq.json` | rpc-messages | 通过 | 非缺陷（规则 BEH-06 承担） |
| `frames/confidence-out-of-range.json` | rpc-messages | 通过 | 非缺陷（规则 BEH-01 承担） |
| `frames/progress-percent-out-of-range.json` | rpc-messages | 拒绝 | 正判（契约 §3.3） |
| `frames/record-null-optional-field.json` | rpc-messages | 拒绝 | 正判（契约 §3.1 skip-if-empty） |
| `frames/error-code-out-of-set.json` | rpc-messages | 拒绝 | 正判（契约 §4.2；validator 折算 BEH-03） |
| `frames/response-with-result-and-error.json` | rpc-messages | 拒绝 | 正判（oneOf 判别联合） |
| `frames/request-id-string.json` | rpc-messages | 拒绝 | 正判（契约 §1.4 RequestId） |
| `frames/ok-record-batch-with-optional-fields.json` | rpc-messages | 通过 | 正例控制（不得 false reject） |
| `frames/ok-progress-omitted-optionals.json` | rpc-messages | 通过 | 正例控制（skip-if-empty 省略合法） |

期望翻转 = 契约修订信号：修订获批后必须同步更新本表与 schema-errata.md
（保持「复现矩阵 ↔ errata」双写一致）。

## 人工过签工作流（D1/D2 合入后执行）

1. 构建校验器：`cargo build --release`；
2. 对四个插件逐一回放并把结果落档（demo-tool 示例）：

```powershell
.\target\release\plugin-check.exe ..\..\plugins\demo-tool --behavior --json > ..\..\tools\plugin-validator\tests\schema_feedback\demo-tool.json
```

3. 对照 `docs/developer-guide/schema-errata.md` 逐条过签：新增差异 → 按四分类
   补 errata 条目；判定为 Schema/契约缺陷 → 用 `contract-change-proposal-template.md`
   提变更提案（主代理审批 + 全路广播），未获批前 docs/spec 零改动。

> 依赖状态：`demo-tool.json` 等插件回放产物待 D1-02/D1-03/D2-02/D2-03 合入后
> 生成（当前 `plugins/` 仅 .gitkeep 占位）。`cargo test schema_feedback` 不依赖
> 上述产物，可随时常备回归。

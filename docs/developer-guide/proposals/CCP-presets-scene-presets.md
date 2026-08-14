# 契约变更提案：场景预设（Scene Presets）addendum

> 依据 `contract-change-proposal-template.md` 成文。与勘误条目不同，本提案属
> **契约扩展（addendum）**：`contract-v1` 冻结正文保持逐字不动，仅在
> `plugin.json` 契约上追加一个可选字段 `presets`，不改动任何既有字段/语义。

---

## 提案基本信息

- **提案编号**：CCP-presets（场景预设）
- **关联勘误条目**：无（非勘误；属冻结契约的 addendum 扩展，不触发既有规则变更）
- **提出人 / 路**：C1 Doc / Wave 1 场景预设（Preset）功能路
- **提出日期**：2026-08-15

## 1. 变更动机

实测现象/需求：AnalysisBuddy 的指标选择是「metricTree 逐文件勾选」的细粒度交互，
测试场景（如「性能总览」「帧率曲线」）所需的指标集合在每次切换文件/会话后都要
重新勾选。为支撑「场景预设」功能，需要插件在 manifest 中声明可选的场景预设
（一组描述「本场景关心哪些指标」的条目），宿主据此在 apply 时把预设解析为该插件
的 metric 勾选集。协议正本（contract-v1 冻结）与 schema 均无此字段，属纯粹的新增，
不影响任何既有帧/字段。

## 2. 影响路清单

| 受影响方 | 影响内容 | 同步动作 |
|----------|----------|----------|
| docs/spec 契约文件 | `plugin-manifest.schema.json`：`#/properties/presets`（追加）+ `#/definitions/localized_name`、`#/definitions/preset_entry`（新增顶层） | 随审批同批落地（addendum 追加，`additionalProperties: true` 不动） |
| ab-protocol 类型 | `Preset`/`PresetGroup`/`PresetEntry`/`LocalizedName`（仅序列化侧，可选字段，null 即无） | 广播确认；不参与协议帧编解码 |
| validator 规则 | 无既有规则变更；新增可选字段仅结构阶段 Schema 校验，非法预设「丢弃该预设+诊断」、不拒插件（宿主侧语义，validator 不新增 error 级规则） | 广播确认 |
| SDK（D1/D2） | 序列化/校验行为不变（未知字段本就被 `additionalProperties` 放行） | 广播确认 |
| 开发者指南（E-01） | `04-manifest-reference.md`、`02-write-a-plugin.md` 新增 presets 章节 | 随契约广播同步 |

## 3. 兼容性论证

- **前向兼容**：`presets` 为可选字段；不提供它的既有插件不受任何影响，旧 manifest
  在新 Schema 下依然通过（schema 只追加、不收紧）。
- **后向兼容**：携带 `presets` 的新 manifest 在旧 Schema 下会被 `additionalProperties:
  true` 放行（旧宿主忽略即可）；任何宿主实现都不被要求支持本字段。
- **规则影响**：规则 ID 无变更（禁止重排/改级；不新增 MAN-xx error，非法预设仅
  宿主侧丢弃 + 诊断）。
- **回归**：既有复现用例集（`tests/schema_feedback/`）期望零翻转；新增正例
  `docs/spec/examples/manifest-ok-presets.json` 必须在改后 Schema 下通过。

## 4. 回滚方式

- 回滚：`git revert` 本提案关联提交（schema 的 `presets`/`definitions` 追加、
  protocol-v1.md 的 addendum 小节、两份指南新增章节、正例 JSON）。
- 回滚后：示例文件 `manifest-ok-presets.json` 一并删除；复现用例集不受影响
  （无翻转）；errata 无需联动（本提案无对应勘误条目）。

## 5. 审批记录

| 审批人 | 结论（通过/驳回/需修改） | 日期 | 备注 |
|--------|--------------------------|------|------|
| 主代理 | | | |
| 受影响路代表（D1/D2/宿主） | | | |

---

## 附：契约冻结包要点（C1 Doc 执行基准，逐字执行）

- **A. JSON Schema**：`properties` 末尾追加 `presets`（array，≤32；条目
  `{id, name, description?, entries?, groups?, keywords?}`；`preset_entry` 为
  `{want?, names[]}`，`localized_name` 为 `{zh, en}` 双语必填）；新增顶层
  `definitions`（`localized_name`、`preset_entry`）。
- **B. 匹配语义**：三级匹配——① 穷举精确（metric_id 精确 → name 大小写不敏感，
  同 want 首个命中，记录命中分组）→ ② keywords 子串模糊兜底（matched_by=fuzzy，
  仅穷举整体零命中时启用）→ ③ 零命中：不改动当前选择，清单化提示 unmatched。
  上限：单插件预设 ≤32、单预设条目 ≤1000；非法预设仅丢弃该预设 + 诊断，不拒插件。
  用户保存预设从 selectedMetrics 反推 plugin_id + metric_id（天然精确）；UI 选择态
  为复合 id `file_id:plugin_id:metric_id`，预设只存 plugin_id + metric 键，应用时
  按当前 metricTree 逐文件解析。预设是场景无关的通用机制，核心不做内建场景假设。

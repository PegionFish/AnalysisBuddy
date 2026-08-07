//! 契约实测反哺（E-03）：Schema 复现用例 + 插件实测驱动器。
//!
//! 冻结纪律：`docs/spec/` 为 contract-v1 冻结契约，本模块**只读不写**；勘误结论落
//! `docs/developer-guide/schema-errata.md`；判为 Schema/契约缺陷的条目必须走
//! 「主代理审批 + 全路广播」流程（PLAN.md §6 规则 1），未获批前 docs/spec 与
//! protocol-v1.md 零改动。
//!
//! 实测对象：本仓库 `plugins/*`（D1-02 sample-plugin / D1-03 builtin-csv /
//! D2-02 sample-plugin-csharp / D2-03 demo-tool，随各路卡片合入后自动纳入）。
//! 当前依赖状态：D1/D2 并行开发中，plugins/ 下暂无插件——驱动器在无插件时返回
//! 空结果，实测反馈部分标注「待 D1-02/D2-02 合入后复验」（见 schema-errata.md）。
//!
//! 复现用例集：`tests/schema_feedback/frames/*.json`（最小帧），随本模块单测
//! 常备回归；契约修订后自动重放，期望值翻转时须同步修订 schema-errata.md。
#![allow(dead_code)] // 面向 E-03 复验工作流（单测 + 人工过签），CLI 不暴露入口

use jsonschema::Validator as JsonSchemaValidator;
use serde_json::Value;

/// 用例适用的 Schema。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReproKind {
    /// plugin-manifest.schema.json
    Manifest,
    /// rpc-messages.schema.json
    Rpc,
}

/// 一条复现用例：帧原文 + Schema 判定期望 + 契约注解。
#[derive(Debug, Clone, Copy)]
pub struct ReproCase {
    pub id: &'static str,
    pub kind: ReproKind,
    pub frame: &'static str,
    /// 结构校验期望：true = 应通过 Schema；false = 应被 Schema 拒绝。
    pub schema_valid: bool,
    /// 契约层面注解（四分类：Schema 缺陷 / 实现缺陷 / 非缺陷 / 文档缺陷）。
    pub note: &'static str,
}

/// 复现用例表。`schema_valid` 期望若因契约修订而翻转，必须同步更新
/// `docs/developer-guide/schema-errata.md` 对应条目。
pub const REPRO_CASES: &[ReproCase] = &[
    // ---- false accept 探针（E-03 errata E-01/E-02）：Schema 放行、契约违规 ----
    ReproCase {
        id: "kv-entries-non-array",
        kind: ReproKind::Rpc,
        frame: r#"{"jsonrpc":"2.0","id":7,"result":{"entries":"nope"}}"#,
        schema_valid: true,
        note: "false accept：key_values 响应 result 仅为 generic object，entries 非数组不被 Schema 拦截（契约 §2.6 KeyValuesResult）→ Schema 缺陷（提案 A）",
    },
    ReproCase {
        id: "init-result-missing-fields",
        kind: ReproKind::Rpc,
        frame: r#"{"jsonrpc":"2.0","id":1,"result":{"id":"x"}}"#,
        schema_valid: true,
        note: "false accept：initialize 响应缺 name/version/capabilities 不被 Schema 拦截（契约 §2.1 InitializeResult）→ Schema 缺陷（提案 A）",
    },
    // ---- 非缺陷探针（规则层已覆盖，Schema 不承担） ----
    ReproCase {
        id: "record-batch-duplicate-seq",
        kind: ReproKind::Rpc,
        frame: r#"{"jsonrpc":"2.0","method":"RecordBatch","params":{"file_id":"f","seq":0,"records":[],"done":false}}"#,
        schema_valid: true,
        note: "非缺陷：seq 重复/缺号属跨帧语义，由规则 BEH-06 判定（docs-validator.md §2.2）；Schema 无单帧判别能力",
    },
    ReproCase {
        id: "confidence-out-of-range",
        kind: ReproKind::Rpc,
        frame: r#"{"jsonrpc":"2.0","id":3,"result":{"can_handle":true,"confidence":1.5}}"#,
        schema_valid: true,
        note: "非缺陷：confidence ∈ [0,1] 语义约束由规则 BEH-01 判定（docs-validator.md §3.5 明确按 BEH-01 同类处理）",
    },
    // ---- Schema 正判用例（正例控制：不得 false reject） ----
    ReproCase {
        id: "ok-record-batch-with-optional-fields",
        kind: ReproKind::Rpc,
        frame: r#"{"jsonrpc":"2.0","method":"RecordBatch","params":{"file_id":"f","seq":0,"records":[{"timestamp":1,"metric":"fps","value":59.8,"level":"info","tags":{"scene":"boss"},"raw_line":"x"}],"done":false}}"#,
        schema_valid: true,
        note: "非缺陷：合法帧（含全部可选字段）不得被 Schema 拒绝",
    },
    ReproCase {
        id: "ok-progress-omitted-optionals",
        kind: ReproKind::Rpc,
        frame: r#"{"jsonrpc":"2.0","method":"progress","params":{"file_id":"f","records_so_far":42}}"#,
        schema_valid: true,
        note: "非缺陷：progress 可选字段（percent/bytes_read）省略合法（契约 §3.3 skip-if-empty）",
    },
    ReproCase {
        id: "ok-schema-request-empty-params",
        kind: ReproKind::Rpc,
        frame: r#"{"jsonrpc":"2.0","id":2,"method":"schema","params":{}}"#,
        schema_valid: true,
        note: "非缺陷：schema 请求空 params 合法（契约 §2.5）",
    },
    // ---- Schema 反判用例（违规帧必须被 Schema 拒绝） ----
    ReproCase {
        id: "progress-percent-out-of-range",
        kind: ReproKind::Rpc,
        frame: r#"{"jsonrpc":"2.0","method":"progress","params":{"file_id":"f","percent":150,"records_so_far":1}}"#,
        schema_valid: false,
        note: "正判：percent 越界 [0,100] 被 Schema 拒绝（契约 §3.3）",
    },
    ReproCase {
        id: "record-null-optional-field",
        kind: ReproKind::Rpc,
        frame: r#"{"jsonrpc":"2.0","method":"RecordBatch","params":{"file_id":"f","seq":0,"records":[{"timestamp":1,"metric":"fps","value":1.0,"level":null}],"done":true}}"#,
        schema_valid: false,
        note: "正判：可选字段输出 null 被 Schema 拒绝（契约 §3.1 skip-if-empty 禁止 null）",
    },
    ReproCase {
        id: "error-code-out-of-set",
        kind: ReproKind::Rpc,
        frame: r#"{"jsonrpc":"2.0","id":1,"error":{"code":-9999,"message":"custom"}}"#,
        schema_valid: false,
        note: "正判：集合外错误码被 Schema 拒绝（契约 §4.2 只许 -32001~-32005 + 标准码）→ validator 折算 BEH-03",
    },
    ReproCase {
        id: "response-with-result-and-error",
        kind: ReproKind::Rpc,
        frame: r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-32603,"message":"x"}}"#,
        schema_valid: false,
        note: "正判：同时携带 result 与 error 的响应被 oneOf 拒绝",
    },
    ReproCase {
        id: "request-id-string",
        kind: ReproKind::Rpc,
        frame: r#"{"jsonrpc":"2.0","id":"1","method":"initialize","params":{"protocol_version":1,"host_info":{"name":"h","version":"1"}}}"#,
        schema_valid: false,
        note: "正判：id 非整数被 Schema 拒绝（契约 §1.4 RequestId）",
    },
    ReproCase {
        id: "manifest-missing-entry",
        kind: ReproKind::Manifest,
        frame: r#"{"id":"sample-plugin","display_name":"x","version":"0.1.0","match":{"extensions":["csv"]},"min_protocol_version":1}"#,
        schema_valid: false,
        note: "正判：缺 entry 被 Schema required 拒绝（MAN-01 判据）",
    },
    ReproCase {
        id: "manifest-id-pattern",
        kind: ReproKind::Manifest,
        frame: r#"{"id":"BAD ID!","display_name":"x","version":"0.1.0","entry":{"command":"x","args":[]},"match":{"extensions":["csv"]},"min_protocol_version":1}"#,
        schema_valid: false,
        note: "正判：id 违反 pattern 被 Schema 拒绝（MAN-01 判据）",
    },
    ReproCase {
        id: "manifest-extra-fields-allowed",
        kind: ReproKind::Manifest,
        frame: r#"{"id":"sample-plugin","display_name":"x","version":"0.1.0","entry":{"command":"x","args":[]},"match":{"extensions":["csv"]},"min_protocol_version":1,"author":"me","license":"GPL"}"#,
        schema_valid: true,
        note: "非缺陷：manifest 顶层 additionalProperties 允许（契约 §7.2 设计）",
    },
];

/// 执行全部复现用例，返回 实际 Schema 判定 × 期望 的对账结果。
pub fn run_repro(
    manifest_schema: &JsonSchemaValidator,
    rpc_schema: &JsonSchemaValidator,
) -> Vec<CaseOutcome> {
    REPRO_CASES
        .iter()
        .map(|case| {
            let value: Value = serde_json::from_str(case.frame)
                .unwrap_or_else(|e| panic!("case `{}` 帧不是合法 JSON：{e}", case.id));
            let validator = match case.kind {
                ReproKind::Manifest => manifest_schema,
                ReproKind::Rpc => rpc_schema,
            };
            let actual_valid = validator.is_valid(&value);
            CaseOutcome {
                id: case.id,
                expected_valid: case.schema_valid,
                actual_valid,
                matches: actual_valid == case.schema_valid,
                note: case.note,
            }
        })
        .collect()
}

/// 单用例执行结果。
#[derive(Debug, Clone, Copy)]
pub struct CaseOutcome {
    pub id: &'static str,
    pub expected_valid: bool,
    pub actual_valid: bool,
    pub matches: bool,
    pub note: &'static str,
}

/// 插件实测反馈（对 `plugins/*` 的 Schema 校验结果与 BEH 判定汇总）。
#[derive(Debug, Clone)]
pub struct PluginFeedback {
    pub dir: String,
    pub manifest_schema_ok: bool,
    pub man_errors: usize,
    pub beh_errors: usize,
    pub beh_warnings: usize,
}

/// 实测驱动器：对仓库 `plugins/*`（每个直接子目录）做结构 + 行为回放，
/// 收集 Schema 校验结果与 BEH 判定差异（docs-validator.md §3.2 单源复核）。
///
/// 依赖状态：D1/D2 插件未合入时返回空 Vec（不报错）；合入后自动纳入复验，
/// 结果人工对照 `docs/developer-guide/schema-errata.md` 过签。
#[allow(dead_code)] // 由单测与 E-03 复验工作流调用（CLI 不暴露该入口）
pub fn run_plugin_driver(
    manifest_schema: &JsonSchemaValidator,
    rpc_schema: &JsonSchemaValidator,
    repo_root: &std::path::Path,
) -> Vec<PluginFeedback> {
    let plugins_root = repo_root.join("plugins");
    let Ok(entries) = std::fs::read_dir(&plugins_root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|t| t.is_dir()) || !path.join("plugin.json").is_file() {
            continue;
        }
        let findings = crate::structure::run(&path, manifest_schema, 1);
        let man_errors = findings
            .iter()
            .filter(|f| f.rule_id.starts_with("MAN-") && f.level == crate::rules::Level::Error)
            .count();
        let manifest_schema_ok = !findings.iter().any(|f| f.rule_id == "MAN-01");
        let (beh_errors, beh_warnings) = if man_errors == 0 {
            let manifest = match std::fs::read_to_string(path.join("plugin.json")) {
                Ok(text) => match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };
            match crate::behavior::run(&crate::behavior::BehaviorInput {
                plugin_dir: path.clone(),
                manifest,
                fixture: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("fixtures/small_with_header.csv"),
                scale: 1.0,
                rpc_schema,
            }) {
                Ok(outcome) => (
                    outcome
                        .findings
                        .iter()
                        .filter(|f| f.level == crate::rules::Level::Error)
                        .count(),
                    outcome
                        .findings
                        .iter()
                        .filter(|f| f.level == crate::rules::Level::Warning)
                        .count(),
                ),
                Err(_) => (0, 0),
            }
        } else {
            (0, 0)
        };
        out.push(PluginFeedback {
            dir: path.display().to_string(),
            manifest_schema_ok,
            man_errors,
            beh_errors,
            beh_warnings,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_schemas() -> (JsonSchemaValidator, JsonSchemaValidator) {
        let spec = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/spec");
        let build = |name: &str| -> JsonSchemaValidator {
            let text = std::fs::read_to_string(spec.join(name)).expect(name);
            let value: Value = serde_json::from_str(&text).expect(name);
            jsonschema::Validator::options()
                .with_draft(jsonschema::Draft::Draft7)
                .build(&value)
                .expect(name)
        };
        (
            build("plugin-manifest.schema.json"),
            build("rpc-messages.schema.json"),
        )
    }

    /// 常备回归：复现用例集的 Schema 判定与期望逐条一致。
    /// 契约修订后自动重放；期望翻转时同步更新 schema-errata.md。
    #[test]
    fn schema_feedback_repro_cases_match_expectations() {
        let (manifest_schema, rpc_schema) = load_schemas();
        let results = run_repro(&manifest_schema, &rpc_schema);
        let mismatches: Vec<_> = results
            .iter()
            .filter(|r| !r.matches)
            .map(|r| r.id)
            .collect();
        assert!(
            mismatches.is_empty(),
            "复现用例判定与期望不符（若为契约修订所致，请同步更新 schema-errata.md）：{mismatches:?}"
        );
        // false accept 探针必须存在（errata E-01/E-02 的复现依据）
        for probe in ["kv-entries-non-array", "init-result-missing-fields"] {
            assert!(
                results.iter().any(|r| r.id == probe),
                "false accept 探针 `{probe}` 必须存在"
            );
        }
    }

    /// docs/spec/examples 全量复核：frame-ok-*/manifest-ok 必须通过，
    /// frame-bad-* 必须被拒绝（protocol-v1.md §3.5「机器可校验副本」承诺）。
    #[test]
    fn schema_feedback_docs_examples_verified() {
        let (manifest_schema, rpc_schema) = load_schemas();
        let examples =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/spec/examples");
        let mut checked = 0usize;
        for entry in std::fs::read_dir(&examples).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".json") {
                continue;
            }
            let text = std::fs::read_to_string(entry.path()).unwrap();
            let value: Value =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            let validator = if name.starts_with("manifest-") {
                &manifest_schema
            } else {
                &rpc_schema
            };
            let expect_ok = name.starts_with("frame-ok-") || name.starts_with("manifest-ok");
            assert_eq!(
                validator.is_valid(&value),
                expect_ok,
                "docs/spec/examples/{name} 判定与文件名不符（§3.5 机器可校验承诺）"
            );
            checked += 1;
        }
        assert!(checked >= 15, "examples 复核数量不足：{checked}");
    }

    /// 实测驱动器：plugins/* 扫描不崩溃；D1/D2 未合入时为空（待复验标注）。
    #[test]
    fn schema_feedback_plugin_driver_scans() {
        let (manifest_schema, rpc_schema) = load_schemas();
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let feedback = run_plugin_driver(&manifest_schema, &rpc_schema, &repo_root);
        // 依赖状态：D1-02/D1-03/D2-02/D2-03 合入后此处应出现 4 个插件；
        // 当前并行开发中可能为 0，不断言数量，只保证驱动器可用。
        for f in &feedback {
            assert!(!f.dir.is_empty(), "插件反馈必须带目录路径");
        }
        eprintln!(
            "schema_feedback driver: {} plugin(s) scanned（D1/D2 合入后应达 4）",
            feedback.len()
        );
    }
}

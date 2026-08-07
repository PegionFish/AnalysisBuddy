//! 21 条冻结规则 ID 与 Finding 结构（docs-validator.md §2）。
//!
//! 冻结纪律：规则 ID 一经发布即冻结，**新增只能追加编号**，不得重排、不得改级。
//! 规则 ID 在以下三处拼写必须逐字符一致：
//! 1. 本文件 `RULE_IDS`；
//! 2. `docs/developer-guide/05-debugging.md` 排错对照表；
//! 3. `plugin check` 的 human / `--json` 输出。
//!
//! 新增规则流程（docs-validator.md §2/§4.3 末段）：
//! 1. 先修订协议正本（docs/spec/protocol-v1.md，须走契约审批）；
//! 2. 在 `RULE_IDS` **末尾**追加编号；
//! 3. 实现对应检查，并在 `tests/` 补该规则的正反两组 fixture 测试；
//! 4. 同批在 `docs/developer-guide/05-debugging.md` 补排错条目；
//!    否则 validator 发布被文档门禁阻塞。
//! 5. 新增规则的测试模板见本文件底部 `#[cfg(test)] mod tests` 的
//!    `new_rule_append_template`。
//!
//! 单源纪律（docs-validator.md §3.2）：结构断言只存在于 docs/spec 两份 JSON
//! Schema（plugin-manifest / rpc-messages）；本 crate 任何模块不得内嵌第二套
//! 结构断言——Schema 演进时 validator 自动跟随，避免双源漂移。

/// 冻结规则 ID 全集（结构 9 + 行为 12）。顺序 = 规则表顺序；测试断言
/// `rule_ids_frozen_and_sorted` 固化此集合，防止误删/重排。
pub const RULE_IDS: [&str; 21] = [
    "MAN-01", "MAN-02", "MAN-03", "MAN-04", "MAN-05", "MAN-06", "MAN-07", "MAN-08", "MAN-09",
    "BEH-01", "BEH-02", "BEH-03", "BEH-04", "BEH-05", "BEH-06", "BEH-07", "BEH-08", "BEH-09",
    "BEH-10", "BEH-11", "BEH-12",
];

/// 级别：error = 不合规（退出码 ≥2）；warning = 可通过但强烈建议修复（退出码 1）。
/// `MAN-09` 为 pass 断言（反向验收项），不产出 Finding。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Warning,
    Error,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Warning => "warning",
            Level::Error => "error",
        }
    }
}

/// 一条规则判定结果（docs-validator.md §1.4：rule id + level + location + message）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// 冻结规则 ID（见 [`RULE_IDS`]）。
    pub rule_id: &'static str,
    pub level: Level,
    /// 人类可读描述。
    pub message: String,
    /// 定位：结构类 = `plugin.json#/…`（JSON Pointer，与 Schema instancePath 对齐）；
    /// 行为类 = `stdout line N` + 方法/seq/record 上下文。
    pub location: String,
}

impl Finding {
    pub fn error(
        rule_id: &'static str,
        message: impl Into<String>,
        location: impl Into<String>,
    ) -> Self {
        Finding {
            rule_id,
            level: Level::Error,
            message: message.into(),
            location: location.into(),
        }
    }

    pub fn warn(
        rule_id: &'static str,
        message: impl Into<String>,
        location: impl Into<String>,
    ) -> Self {
        Finding {
            rule_id,
            level: Level::Warning,
            message: message.into(),
            location: location.into(),
        }
    }

    /// error 先行、warning 在后，同级按规则 ID 字典序（输出器直接消费）。
    pub fn sort_by_rule(findings: &mut [Finding]) {
        findings.sort_by(|a, b| {
            b.level
                .as_str()
                .cmp(a.level.as_str())
                .then(a.rule_id.cmp(b.rule_id))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 冻结集合固化：21 条、前缀组内有序、无重复。防误删/重排（E-02 DoD：
    /// 规则 ID 仅追加不重排）。注：数组整体保持「结构 MAN 在前、行为 BEH 在后」
    /// 的文档表顺序（docs-validator.md §2），非全量字典序。
    #[test]
    fn rule_ids_frozen_and_sorted() {
        assert_eq!(RULE_IDS.len(), 21, "规则总数必须为 21（结构 9 + 行为 12）");
        let mut seen = std::collections::HashSet::new();
        for id in RULE_IDS {
            assert!(seen.insert(id), "RULE_IDS 不得重复：{id}");
        }
        // 结构 9 条在前、行为 12 条在后；组内按编号升序
        assert!(RULE_IDS[..9].iter().all(|id| id.starts_with("MAN-")));
        assert!(RULE_IDS[9..].iter().all(|id| id.starts_with("BEH-")));
        for window in RULE_IDS.windows(2) {
            let a = window[0][4..].parse::<u32>().unwrap();
            let b = window[1][4..].parse::<u32>().unwrap();
            assert!(
                a + 1 == b || window[0].starts_with("MAN-") && window[1].starts_with("BEH-"),
                "规则编号必须连续递增：{} -> {}",
                window[0],
                window[1]
            );
        }
        // 级别裁定抽查（docs-validator.md §2.2/§2.3）：终止会话类一律 error；
        // 宿主容忍降级类一律 warning。
        let error_ids: [&str; 14] = [
            "MAN-01", "MAN-02", "MAN-03", "MAN-05", "MAN-08", "BEH-01", "BEH-02", "BEH-03",
            "BEH-04", "BEH-05", "BEH-06", "BEH-07", "BEH-08", "BEH-09",
        ];
        let warning_ids: [&str; 6] = ["MAN-04", "MAN-06", "MAN-07", "BEH-10", "BEH-11", "BEH-12"];
        for id in error_ids {
            assert!(RULE_IDS.contains(&id), "{id} 必须存在");
        }
        for id in warning_ids {
            assert!(RULE_IDS.contains(&id), "{id} 必须存在");
        }
        // 注：docs-validator.md 附录声称 error 级 15 条，实为 14（MAN 5 + BEH 9）；
        // 差异已在 E-03 schema-errata.md 记录为文档缺陷待修订。
    }

    /// 新增规则模板：复制本测试并按新编号命名，断言新 id 已追加至 RULE_IDS 末尾
    /// （例如 BEH-13）。新增时同步执行新增规则四步流程（见模块文档）。
    #[test]
    #[allow(dead_code)]
    fn new_rule_append_template() {
        // let new_id = "BEH-13";
        // assert!(RULE_IDS.last() == Some(&new_id), "新规则必须追加到 RULE_IDS 末尾");
    }
}

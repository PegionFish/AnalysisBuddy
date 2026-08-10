//! human / json 双渲染器（docs-validator.md §1.4），共用同一 `Finding` 结构。
//!
//! `--json` 输出顶层字段冻结：`plugin_dir` / `rules` / `summary` / `exit_code`；
//! `rules[]` 字段冻结：`id` / `level` / `message` / `location`。

use std::path::Path;

use serde_json::{json, Value};

use crate::rules::{Finding, Level};

/// Phase 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseStatus {
    Pass,
    Fail,
    Skipped,
}

impl PhaseStatus {
    fn as_str(&self) -> &'static str {
        match self {
            PhaseStatus::Pass => "pass",
            PhaseStatus::Fail => "fail",
            PhaseStatus::Skipped => "skipped",
        }
    }
}

/// 渲染输入（main 组装）。
pub struct Report<'a> {
    pub plugin_dir: &'a Path,
    pub findings: &'a [Finding],
    pub passed_rules: Vec<&'static str>,
    pub phase1: PhaseStatus,
    pub phase2: PhaseStatus,
    pub notes: Vec<String>,
    pub stderr_dump: Option<String>,
    pub exit_code: u32,
}

impl Report<'_> {
    /// 人类可读渲染：error → warning → pass 摘要分级，每条带定位。
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "== plugin check: {} ==\n\n",
            self.plugin_dir.display()
        ));
        for f in self.findings {
            let tag = match f.level {
                Level::Error => "ERROR",
                Level::Warning => "WARN ",
            };
            out.push_str(&format!("[{tag}] {}  {}\n", f.rule_id, f.message));
            out.push_str(&format!("        at {}\n", f.location));
        }
        let errors = self
            .findings
            .iter()
            .filter(|f| f.level == Level::Error)
            .count();
        let warnings = self
            .findings
            .iter()
            .filter(|f| f.level == Level::Warning)
            .count();
        if !self.passed_rules.is_empty() {
            out.push_str(&format!(
                "\n[PASS]  {} ({} rules)\n",
                compact_ids(&self.passed_rules),
                self.passed_rules.len()
            ));
        }
        out.push_str(&format!(
            "\nSummary: {errors} error(s), {warnings} warning(s) -> exit code {}\n",
            self.exit_code
        ));
        for note in &self.notes {
            out.push_str(&format!("注: {note}\n"));
        }
        if let Some(path) = &self.stderr_dump {
            out.push_str(&format!("注: stderr 已转储到 {path}\n"));
        }
        out
    }

    /// `--json` 渲染：顶层字段名冻结（docs-validator.md §1.4）。
    pub fn render_json(&self) -> Value {
        let rules: Vec<Value> = self
            .findings
            .iter()
            .map(|f| {
                json!({
                    "id": f.rule_id,
                    "level": f.level.as_str(),
                    "message": f.message,
                    "location": f.location,
                })
            })
            .collect();
        let errors = self
            .findings
            .iter()
            .filter(|f| f.level == Level::Error)
            .count();
        let warnings = self
            .findings
            .iter()
            .filter(|f| f.level == Level::Warning)
            .count();
        json!({
            "plugin_dir": self.plugin_dir.display().to_string(),
            "rules": rules,
            "summary": {
                "errors": errors,
                "warnings": warnings,
                "passed_rules": self.passed_rules,
                "phase1": self.phase1.as_str(),
                "phase2": self.phase2.as_str(),
                "notes": self.notes,
                "stderr_dump": self.stderr_dump,
            },
            "exit_code": self.exit_code,
        })
    }
}

/// 规则 ID 区间压缩：`MAN-01, MAN-02, MAN-03` → `MAN-01..MAN-03`。
/// 输入必须已按字典序排序。
pub fn compact_ids(ids: &[&str]) -> String {
    let mut groups: Vec<Vec<&str>> = Vec::new();
    for &id in ids {
        if let Some(last) = groups.last_mut() {
            if let Some(&last_id) = last.last() {
                if prev_prefix(last_id) == prev_prefix(id) && is_next_number(last_id, id) {
                    last.push(id);
                    continue;
                }
            }
        }
        groups.push(vec![id]);
    }
    groups
        .iter()
        .map(|g| {
            if g.len() >= 3 {
                format!("{}..{}", g[0], g[g.len() - 1])
            } else {
                g.join(", ")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn prev_prefix(id: &str) -> Option<&str> {
    let idx = id.rfind(|c: char| !c.is_ascii_digit())?;
    Some(&id[..=idx])
}

fn is_next_number(prev: &str, cur: &str) -> bool {
    let (Some(prefix), Some(cur_prefix)) = (prev_prefix(prev), prev_prefix(cur)) else {
        return false;
    };
    if prefix != cur_prefix {
        return false;
    }
    let (Ok(pn), Ok(cn)) = (
        prev[prefix.len()..].parse::<u64>(),
        cur[prefix.len()..].parse::<u64>(),
    ) else {
        return false;
    };
    cn == pn + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_range_compaction() {
        let ids = [
            "BEH-01", "BEH-02", "BEH-03", "BEH-10", "BEH-11", "MAN-01", "MAN-05",
        ];
        assert_eq!(
            compact_ids(&ids),
            "BEH-01..BEH-03, BEH-10, BEH-11, MAN-01, MAN-05"
        );
        assert_eq!(compact_ids(&["MAN-01", "MAN-02"]), "MAN-01, MAN-02");
        let long = [
            "MAN-01", "MAN-02", "MAN-03", "MAN-04", "MAN-05", "MAN-06", "MAN-07", "MAN-08",
            "MAN-09",
        ];
        assert_eq!(compact_ids(&long), "MAN-01..MAN-09");
    }

    #[test]
    fn json_top_level_fields_frozen() {
        let finding = Finding::error("BEH-02", "response id mismatch", "stdout line 7");
        let report = Report {
            plugin_dir: Path::new(r"C:\plugins\demo"),
            findings: std::slice::from_ref(&finding),
            passed_rules: vec!["MAN-01", "BEH-01"],
            phase1: PhaseStatus::Pass,
            phase2: PhaseStatus::Skipped,
            notes: vec![],
            stderr_dump: None,
            exit_code: 0,
        };
        let v = report.render_json();
        let obj = v.as_object().expect("json object");
        for key in ["plugin_dir", "rules", "summary", "exit_code"] {
            assert!(
                obj.contains_key(key),
                "顶层字段 `{key}` 必须存在（冻结形态）"
            );
        }
        assert_eq!(obj.keys().count(), 4, "顶层字段不得增删");
        let rule = &obj["rules"][0];
        for key in ["id", "level", "message", "location"] {
            assert!(
                rule.as_object().unwrap().contains_key(key),
                "rules[].{key} 必须存在"
            );
        }
        assert_eq!(rule.as_object().unwrap().keys().count(), 4);
    }
}

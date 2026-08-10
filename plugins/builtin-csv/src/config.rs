//! 插件私有配置（sdk-plugins.md §3.2，protocol.md §7.1 规则 4）。
//!
//! 配置文件路径：插件目录下 `config.json`（不存在则全默认）。**不通过
//! `parse.options` 传**。非法值 → stderr 告警并按该键默认值回落。

use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

/// time_format.kind 四态。
#[derive(Debug, Clone, PartialEq)]
pub enum TimeFormat {
    Auto,
    EpochMs,
    Iso8601,
    Custom(String),
}

/// has_header 三态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HasHeader {
    Auto,
    Yes,
    No,
}

/// delimiter 四态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Delimiter {
    Auto,
    Comma,
    Semicolon,
    Tab,
}

impl Delimiter {
    pub fn as_char(self) -> Option<char> {
        match self {
            Delimiter::Comma => Some(','),
            Delimiter::Semicolon => Some(';'),
            Delimiter::Tab => Some('\t'),
            Delimiter::Auto => None,
        }
    }
}

/// encoding 五态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Encoding {
    Auto,
    Utf8,
    Utf16Le,
    Utf16Be,
    Gbk,
}

/// 五键配置；缺省全默认（与 `config.json` 示例一致）。
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub time_column: String,
    pub time_format: TimeFormat,
    pub has_header: HasHeader,
    pub delimiter: Delimiter,
    pub encoding: Encoding,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            time_column: "timestamp".to_string(),
            time_format: TimeFormat::Auto,
            has_header: HasHeader::Auto,
            delimiter: Delimiter::Auto,
            encoding: Encoding::Auto,
        }
    }
}

/// 宽松 raw 形态（全部可选）；逐键校验，非法值回落默认。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    time_column: Option<String>,
    time_format: Option<serde_json::Value>,
    has_header: Option<serde_json::Value>,
    delimiter: Option<String>,
    encoding: Option<String>,
}

fn warn(warnings: &mut Vec<String>, msg: impl Into<String>) {
    warnings.push(msg.into());
}

/// 从 JSON 文本解析配置（纯函数，可单测）；`warnings` 收集逐键回落原因。
pub fn parse_config(text: &str, warnings: &mut Vec<String>) -> Config {
    let raw: Result<RawConfig, _> = serde_json::from_str(text);
    let raw = match raw {
        Ok(raw) => raw,
        Err(e) => {
            warn(
                warnings,
                format!("config.json malformed ({e}); using defaults"),
            );
            return Config::default();
        }
    };
    let mut cfg = Config::default();

    if let Some(tc) = raw.time_column {
        let tc = tc.trim();
        if tc.is_empty() {
            warn(warnings, "config: time_column empty; using default");
        } else {
            cfg.time_column = tc.to_string();
        }
    }

    if let Some(tf) = raw.time_format {
        let kind = tf
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("auto");
        match kind {
            "auto" => cfg.time_format = TimeFormat::Auto,
            "epoch_ms" => cfg.time_format = TimeFormat::EpochMs,
            "iso8601" => cfg.time_format = TimeFormat::Iso8601,
            "custom" => {
                let pattern = tf.get("pattern").and_then(serde_json::Value::as_str);
                match pattern {
                    Some(p) if !p.is_empty() => cfg.time_format = TimeFormat::Custom(p.to_string()),
                    _ => {
                        warn(
                            warnings,
                            "config: time_format custom requires non-empty pattern; using auto",
                        );
                        cfg.time_format = TimeFormat::Auto;
                    }
                }
            }
            other => {
                warn(
                    warnings,
                    format!("config: unknown time_format.kind `{other}`; using auto"),
                );
                cfg.time_format = TimeFormat::Auto;
            }
        }
    }

    if let Some(hh) = raw.has_header {
        match hh {
            serde_json::Value::Bool(b) => {
                cfg.has_header = if b { HasHeader::Yes } else { HasHeader::No }
            }
            serde_json::Value::String(s) if s.eq_ignore_ascii_case("auto") => {
                cfg.has_header = HasHeader::Auto
            }
            other => warn(
                warnings,
                format!("config: invalid has_header {other}; using auto"),
            ),
        }
    }

    if let Some(d) = raw.delimiter {
        match d.as_str() {
            "auto" => cfg.delimiter = Delimiter::Auto,
            "," => cfg.delimiter = Delimiter::Comma,
            ";" => cfg.delimiter = Delimiter::Semicolon,
            "\t" | "\\t" => cfg.delimiter = Delimiter::Tab,
            other => warn(
                warnings,
                format!("config: invalid delimiter `{other}`; using auto"),
            ),
        }
    }

    if let Some(e) = raw.encoding {
        match e.as_str() {
            "auto" => cfg.encoding = Encoding::Auto,
            "utf-8" | "utf8" => cfg.encoding = Encoding::Utf8,
            "utf-16le" | "utf16le" => cfg.encoding = Encoding::Utf16Le,
            "utf-16be" | "utf16be" => cfg.encoding = Encoding::Utf16Be,
            "gbk" => cfg.encoding = Encoding::Gbk,
            other => warn(
                warnings,
                format!("config: invalid encoding `{other}`; using auto"),
            ),
        }
    }

    cfg
}

/// 加载插件目录下 `config.json`；查找顺序：当前目录 → 可执行文件所在目录。
pub fn load_config() -> (Config, Vec<String>) {
    let mut warnings = Vec::new();
    for dir in [
        std::env::current_dir().ok(),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from)),
    ]
    .into_iter()
    .flatten()
    {
        let path = dir.join("config.json");
        if let Ok(text) = fs::read_to_string(&path) {
            let cfg = parse_config(&text, &mut warnings);
            return (cfg, warnings);
        }
    }
    (Config::default(), warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_absent_or_malformed() {
        let mut w = Vec::new();
        let cfg = parse_config("", &mut w);
        assert_eq!(cfg, Config::default());
        let cfg = parse_config("not json at all", &mut w);
        assert_eq!(cfg, Config::default());
        assert!(w.iter().any(|m| m.contains("malformed")));
    }

    #[test]
    fn all_five_keys_full_path() {
        let text = r#"{
            "time_column": "t",
            "time_format": { "kind": "custom", "pattern": "%Y/%m/%d %H:%M:%S%.f" },
            "has_header": false,
            "delimiter": ";",
            "encoding": "gbk"
        }"#;
        let mut w = Vec::new();
        let cfg = parse_config(text, &mut w);
        assert!(w.is_empty());
        assert_eq!(cfg.time_column, "t");
        assert_eq!(
            cfg.time_format,
            TimeFormat::Custom("%Y/%m/%d %H:%M:%S%.f".into())
        );
        assert_eq!(cfg.has_header, HasHeader::No);
        assert_eq!(cfg.delimiter, Delimiter::Semicolon);
        assert_eq!(cfg.encoding, Encoding::Gbk);
    }

    #[test]
    fn time_format_states() {
        let mut w = Vec::new();
        for kind in ["auto", "epoch_ms", "iso8601"] {
            let text = format!(r#"{{"time_format": {{"kind": "{}"}}}}"#, kind);
            let cfg = parse_config(&text, &mut w);
            assert!(
                matches!(
                    cfg.time_format,
                    TimeFormat::Auto | TimeFormat::EpochMs | TimeFormat::Iso8601
                ),
                "{kind}"
            );
        }
        // custom 缺 pattern → 回落 auto。
        let cfg = parse_config(r#"{"time_format": {"kind": "custom"}}"#, &mut w);
        assert_eq!(cfg.time_format, TimeFormat::Auto);
        // 未知 kind → 回落 auto。
        let cfg = parse_config(r#"{"time_format": {"kind": "bogus"}}"#, &mut w);
        assert_eq!(cfg.time_format, TimeFormat::Auto);
        // 非法 JSON 键类型 → 回落。
        let cfg = parse_config(r#"{"has_header": 42}"#, &mut w);
        assert_eq!(cfg.has_header, HasHeader::Auto);
    }

    #[test]
    fn has_header_and_delimiter_and_encoding_states() {
        let mut w = Vec::new();
        assert_eq!(
            parse_config(r#"{"has_header": true}"#, &mut w).has_header,
            HasHeader::Yes
        );
        assert_eq!(
            parse_config(r#"{"has_header": "auto"}"#, &mut w).has_header,
            HasHeader::Auto
        );
        assert_eq!(
            parse_config(r#"{"delimiter": ","}"#, &mut w).delimiter,
            Delimiter::Comma
        );
        assert_eq!(
            parse_config(r#"{"delimiter": "\t"}"#, &mut w).delimiter,
            Delimiter::Tab
        );
        assert_eq!(
            parse_config(r#"{"delimiter": "\\t"}"#, &mut w).delimiter,
            Delimiter::Tab
        );
        assert_eq!(
            parse_config(r#"{"encoding": "utf-16le"}"#, &mut w).encoding,
            Encoding::Utf16Le
        );
        assert_eq!(
            parse_config(r#"{"encoding": "utf-16be"}"#, &mut w).encoding,
            Encoding::Utf16Be
        );
        assert_eq!(
            parse_config(r#"{"encoding": "utf-8"}"#, &mut w).encoding,
            Encoding::Utf8
        );
        assert_eq!(
            parse_config(r#"{"encoding": "gbk"}"#, &mut w).encoding,
            Encoding::Gbk
        );
        assert_eq!(
            parse_config(r#"{"delimiter": "x"}"#, &mut w).delimiter,
            Delimiter::Auto
        );
    }

    #[test]
    fn invalid_values_warn_and_fall_back() {
        let mut w = Vec::new();
        let cfg = parse_config(
            r#"{"time_column": "", "time_format": {"kind": "custom"}, "delimiter": "x", "encoding": "y", "has_header": "z"}"#,
            &mut w,
        );
        assert_eq!(cfg, Config::default());
        assert!(w.len() >= 4, "warnings: {w:?}");
    }
}

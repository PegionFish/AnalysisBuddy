//! 极简 CSV 行切分（支持双引号转义，RFC 4180 子集）。
//!
//! 独立实现（不依赖 SDK crate），逻辑与 builtin-csv 的 csvline 模块语义一致：
//! 引号字段可含逗号/换行；`""` 转义；无引号字段照原样返回（由调用方 trim）。

/// 判断一行是否含未闭合引号（多行字段续行用）。
#[cfg(test)]
pub fn has_unclosed_quote(line: &str) -> bool {
    let mut in_quotes = false;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => in_quotes = !in_quotes,
            _ => {}
        }
        i += 1;
    }
    in_quotes
}

/// RFC 4180 子集行切分：引号字段可含分隔符；`""` 转义；字段内换行不支持（AIBench 导出不含引号内换行）。
pub fn split_line(line: &str, delim: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == delim {
            fields.push(cur);
            cur = String::new();
        } else {
            cur.push(c);
        }
    }
    fields.push(cur);
    fields
}

/// 剥除字段两侧空格与包裹引号（`"a,b"` → `a,b`）。
pub fn unquote(field: &str) -> String {
    let t = field.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].replace("\"\"", "\"")
    } else {
        t.to_string()
    }
}

/// 自动探测分隔符：首行 `,` vs `\t` 哪个多（与 builtin-csv 口径一致）。
pub fn auto_delimiter(first_line: &str) -> char {
    let commas = first_line.matches(',').count();
    let tabs = first_line.matches('\t').count();
    if tabs > commas {
        '\t'
    } else {
        ','
    }
}

/// 数字解析：容忍前导 +、千分位逗号、前后空格；空串与纯符号返回 None。
pub fn parse_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let t = t.strip_prefix('+').unwrap_or(t);
    // 千分位逗号："1,234.5" → "1234.5"。仅当整体匹配 ^\d{1,3}(,\d{3})+(\.\d+)?$。
    let has_comma = t.contains(',');
    if has_comma {
        // 拆出小数部分（小数点只在末段允许出现一次）。
        let (int_part, frac_part) = match t.rsplit_once('.') {
            Some((i, f)) => {
                if f.contains('.') {
                    return None;
                }
                (i, Some(f))
            }
            None => (t, None),
        };
        let digits_only: bool = int_part
            .split(',')
            .all(|p| p.chars().all(|c| c.is_ascii_digit()));
        let groups_ok = int_part.split(',').skip(1).all(|p| p.len() == 3);
        let frac_ok = frac_part.map_or(true, |f| {
            !f.is_empty() && f.chars().all(|c| c.is_ascii_digit())
        });
        if digits_only && groups_ok && frac_ok {
            let cleaned = t.replace(',', "");
            return cleaned.parse::<f64>().ok();
        }
        return None;
    }
    t.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_line_basic_and_quotes() {
        assert_eq!(split_line("a,b,c", ','), vec!["a", "b", "c"]);
        assert_eq!(split_line("\"a,b\",c", ','), vec!["a,b", "c"]);
        assert_eq!(
            split_line("\"he said \"\"hi\"\"\",2", ','),
            vec!["he said \"hi\"", "2"]
        );
        assert_eq!(split_line("a\tb\tc", '\t'), vec!["a", "b", "c"]);
    }

    #[test]
    fn unquote_strips_quotes() {
        assert_eq!(unquote(" x "), "x");
        assert_eq!(unquote("\"a,b\""), "a,b");
        assert_eq!(unquote("\""), "\"");
    }

    #[test]
    fn auto_delimiter_prefers_tabs_when_more() {
        assert_eq!(auto_delimiter("a,b,c"), ',');
        assert_eq!(auto_delimiter("a\tb\tc\td"), '\t');
        assert_eq!(auto_delimiter("a,b\tc"), ',');
    }

    #[test]
    fn parse_number_tolerant() {
        assert_eq!(parse_number("123.5"), Some(123.5));
        assert_eq!(parse_number(" 42 "), Some(42.0));
        assert_eq!(parse_number("+7"), Some(7.0));
        assert_eq!(parse_number("1,234.5"), Some(1234.5));
        assert_eq!(parse_number("1,23"), None);
        assert_eq!(parse_number(""), None);
        assert_eq!(parse_number("abc"), None);
        assert_eq!(parse_number("-"), None);
        assert_eq!(parse_number("1 234"), None);
    }

    #[test]
    fn has_unclosed_quote_detects() {
        assert!(!has_unclosed_quote("a,b"));
        assert!(has_unclosed_quote("a,\"b"));
        assert!(!has_unclosed_quote("a,\"b\""));
    }
}

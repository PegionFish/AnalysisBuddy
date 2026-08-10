//! 引号感知 CSV 行切分（RFC4180 子集：`"` 包裹、`""` 转义）。
//!
//! 不引入 csv crate（sdk-plugins.md §3.4，控制依赖面）。

/// 按分隔符切一行；引号内分隔符不切，`""` 转义为字面 `"`。
pub fn split_line(line: &str, delim: char) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cur.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' && cur.is_empty() {
            in_quotes = true;
        } else if c == delim {
            fields.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    fields.push(cur);
    fields
}

/// 首行分隔符自动探测（§3.2：统计 `,` `;` `\t` 出现次数取最大且 ≥1 者，否则 `,`）。
pub fn auto_delimiter(first_line: &str) -> char {
    let mut comma = 0usize;
    let mut semicolon = 0usize;
    let mut tab = 0usize;
    for c in first_line.chars() {
        match c {
            ',' => comma += 1,
            ';' => semicolon += 1,
            '\t' => tab += 1,
            _ => {}
        }
    }
    let max = comma.max(semicolon).max(tab);
    if max == 0 || comma == max {
        ','
    } else if semicolon == max {
        ';'
    } else {
        '\t'
    }
}

/// 宽松数值判断：去引号、去空白后能解析为 f64 即视为数值。
pub fn is_number(cell: &str) -> bool {
    let unq = unquote(cell);
    let t = unq.trim();
    !t.is_empty() && t.parse::<f64>().ok().is_some_and(|v| v.is_finite())
}

/// 去掉首尾引号（若整格被引号包裹）。
pub fn unquote(cell: &str) -> String {
    let t = cell.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].replace("\"\"", "\"")
    } else {
        t.to_string()
    }
}

/// 解析数值（宽松）：失败返回 None。
pub fn parse_number(cell: &str) -> Option<f64> {
    let unq = unquote(cell);
    let t = unq.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_fields() {
        assert_eq!(split_line("a,b,c", ','), vec!["a", "b", "c"]);
        assert_eq!(split_line("a,,c", ','), vec!["a", "", "c"]);
        assert_eq!(split_line("", ','), vec![""]);
        assert_eq!(split_line("a;b;c", ';'), vec!["a", "b", "c"]);
        assert_eq!(split_line("a\tb", '\t'), vec!["a", "b"]);
    }

    #[test]
    fn quoted_fields() {
        assert_eq!(split_line(r#"a,"b,c",d"#, ','), vec!["a", "b,c", "d"]);
        assert_eq!(
            split_line(r#""he said ""hi""",2"#, ','),
            vec!["he said \"hi\"", "2"]
        );
        assert_eq!(
            split_line(r#""leading",trailing"#, ','),
            vec!["leading", "trailing"]
        );
        assert_eq!(split_line(r#""a""b",c"#, ','), vec!["a\"b", "c"]);
    }

    #[test]
    fn empty_and_single() {
        assert_eq!(split_line("a", ','), vec!["a"]);
        assert_eq!(split_line(",", ','), vec!["", ""]);
    }

    #[test]
    fn delimiter_detection() {
        assert_eq!(auto_delimiter("a,b,c"), ',');
        assert_eq!(auto_delimiter("a;b;c"), ';');
        assert_eq!(auto_delimiter("a\tb\tc"), '\t');
        assert_eq!(auto_delimiter("a,b;c"), ',');
        assert_eq!(auto_delimiter("abc"), ',');
        assert_eq!(auto_delimiter(""), ',');
    }

    #[test]
    fn numbers() {
        assert!(is_number("59.8"));
        assert!(is_number(" 59.8 "));
        assert!(is_number(r#""59.8""#));
        assert!(is_number("123"));
        assert!(is_number("1e3"));
        assert!(!is_number("abc"));
        assert!(!is_number(""));
        assert!(!is_number("nan"));
        assert_eq!(parse_number("59.8"), Some(59.8));
        assert_eq!(parse_number(r#""16.6""#), Some(16.6));
        assert_eq!(parse_number("x"), None);
        assert_eq!(parse_number("inf"), None);
    }
}

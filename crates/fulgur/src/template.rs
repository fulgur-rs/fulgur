use crate::error::Result;

/// Insert commas as thousands separators into the integer part of a numeric string.
fn insert_commas(s: &str) -> String {
    let (integer, decimal) = match s.split_once('.') {
        Some((int, dec)) => (int, Some(dec)),
        None => (s, None),
    };

    let negative = integer.starts_with('-');
    let digits = if negative { &integer[1..] } else { integer };

    let mut result = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }

    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(&result);
    if let Some(dec) = decimal {
        out.push('.');
        out.push_str(dec);
    }
    out
}

/// Python-style numeric format filter.
///
/// Supports a subset of Python's format spec for numbers:
/// - `","` — thousands comma separator (integer)
/// - `",.Nf"` — comma separator with N decimal places
/// - `".Nf"` — N decimal places without comma
/// - `"0Nd"` — zero-padded integer to width N
fn format_filter(value: &minijinja::Value, spec: &str) -> String {
    let as_f64 = f64::try_from(value.clone()).ok();
    let as_i64 = i64::try_from(value.clone()).ok();

    // "," — comma-separated integer
    if spec == "," {
        return match as_i64 {
            Some(n) => insert_commas(&n.to_string()),
            None => match as_f64 {
                Some(f) => insert_commas(&f.to_string()),
                None => value.to_string(),
            },
        };
    }

    // ",.Nf" — comma + N decimal places
    if let Some(rest) = spec.strip_prefix(',') {
        if let Some(prec_str) = rest.strip_prefix('.').and_then(|s| s.strip_suffix('f')) {
            if let Ok(prec) = prec_str.parse::<usize>() {
                if let Some(f) = as_f64 {
                    return insert_commas(&format!("{:.prec$}", f));
                }
            }
        }
    }

    // ".Nf" — N decimal places
    if let Some(inner) = spec.strip_prefix('.').and_then(|s| s.strip_suffix('f')) {
        if let Ok(prec) = inner.parse::<usize>() {
            if let Some(f) = as_f64 {
                return format!("{:.prec$}", f);
            }
        }
    }

    // "0Nd" — zero-padded integer
    if spec.starts_with('0') && spec.ends_with('d') {
        if let Ok(width) = spec[..spec.len() - 1].parse::<usize>() {
            if let Some(n) = as_i64 {
                return format!("{:0>width$}", n);
            }
        }
    }

    value.to_string()
}

/// Render a MiniJinja template with JSON data.
pub fn render_template(name: &str, template_str: &str, data: &serde_json::Value) -> Result<String> {
    let mut env = minijinja::Environment::new();
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
    env.add_filter("format", format_filter);
    env.add_template(name, template_str)?;
    let tmpl = env.get_template(name)?;
    Ok(tmpl.render(data)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_variable_substitution() {
        let tmpl = "<h1>{{ title }}</h1>";
        let data = json!({"title": "Hello"});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "<h1>Hello</h1>");
    }

    #[test]
    fn test_loop() {
        let tmpl = "{% for item in items %}<li>{{ item }}</li>{% endfor %}";
        let data = json!({"items": ["a", "b"]});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "<li>a</li><li>b</li>");
    }

    #[test]
    fn test_conditional() {
        let tmpl = "{% if show %}yes{% else %}no{% endif %}";
        let data = json!({"show": true});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "yes");
    }

    #[test]
    fn test_filter() {
        let tmpl = "{{ name | upper }}";
        let data = json!({"name": "hello"});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "HELLO");
    }

    #[test]
    fn test_syntax_error() {
        let tmpl = "{% if %}";
        let data = json!({});
        let result = render_template("test.html", tmpl, &data);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Template error"));
    }

    #[test]
    fn test_empty_data() {
        let tmpl = "<p>static</p>";
        let data = json!({});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "<p>static</p>");
    }

    #[test]
    fn test_html_autoescaping() {
        let tmpl = "{{ text }}";
        let data = json!({"text": "<script>alert(1)</script>"});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert!(!result.contains("<script>"));
        // Also works with non-.html names due to forced autoescape
        let result = render_template("test.txt", tmpl, &data).unwrap();
        assert!(!result.contains("<script>"));
    }

    #[test]
    fn test_undefined_variable_renders_empty() {
        // MiniJinja renders undefined variables as empty string by default
        let tmpl = "{{ missing }}";
        let data = json!({});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_format_comma() {
        let tmpl = r#"{{ n | format(",") }}"#;
        let data = json!({"n": 1234567});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "1,234,567");
    }

    #[test]
    fn test_format_comma_decimal() {
        let tmpl = r#"{{ n | format(",.2f") }}"#;
        let data = json!({"n": 1234567.891});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "1,234,567.89");
    }

    #[test]
    fn test_format_decimal_only() {
        let tmpl = r#"{{ n | format(".2f") }}"#;
        let data = json!({"n": 3.14159});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "3.14");
    }

    #[test]
    fn test_format_zero_pad() {
        let tmpl = r#"{{ n | format("04d") }}"#;
        let data = json!({"n": 5});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "0005");
    }

    #[test]
    fn test_format_comma_negative() {
        let tmpl = r#"{{ n | format(",") }}"#;
        let data = json!({"n": -1234567});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "-1,234,567");
    }

    #[test]
    fn test_format_comma_small() {
        let tmpl = r#"{{ n | format(",") }}"#;
        let data = json!({"n": 42});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "42");
    }

    #[test]
    fn test_invalid_filter() {
        let tmpl = "{{ name | nonexistent_filter }}";
        let data = json!({"name": "hello"});
        let result = render_template("test.html", tmpl, &data);
        assert!(result.is_err());
    }

    #[test]
    fn test_for_loop_over_string_iterates_chars() {
        // MiniJinja iterates over characters of a string
        let tmpl = "{% for c in items %}[{{ c }}]{% endfor %}";
        let data = json!({"items": "ab"});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "[a][b]");
    }

    #[test]
    fn test_unclosed_block() {
        let tmpl = "{% for item in items %}{{ item }}";
        let data = json!({"items": ["a"]});
        let result = render_template("test.html", tmpl, &data);
        assert!(result.is_err());
    }

    #[test]
    fn test_nested_access_missing_key_renders_empty() {
        // MiniJinja renders missing nested keys as empty string
        let tmpl = "{{ user.name }}";
        let data = json!({"user": {}});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "");
    }
}

use crate::error::Result;

/// Render a MiniJinja template with JSON data.
pub fn render_template(name: &str, template_str: &str, data: &serde_json::Value) -> Result<String> {
    let mut env = minijinja::Environment::new();
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
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

use std::collections::BTreeMap;

use minijinja::machinery::{WhitespaceConfig, ast, parse};
use minijinja::syntax::SyntaxConfig;
use serde_json::{Value, json};

/// MiniJinjaテンプレートを解析し、JSON Schemaを生成する。
pub fn extract_schema(template_str: &str, template_name: &str) -> crate::error::Result<Value> {
    let stmt = parse(
        template_str,
        template_name,
        SyntaxConfig,
        WhitespaceConfig::default(),
    )?;

    let mut root = BTreeMap::new();
    let scope = BTreeMap::new();
    collect_from_stmt(&stmt, &mut root, &scope);

    let mut schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "description": format!("Schema for template {}", template_name),
    });

    let properties = inferred_map_to_schema(&root);
    schema["properties"] = properties;

    Ok(schema)
}

/// MiniJinjaテンプレートをサンプルJSONデータと突合し、JSON Schemaを生成する。
/// テンプレートで使用されている変数のみ出力し、型はサンプルデータから確定する。
pub fn extract_schema_with_data(
    template_str: &str,
    template_name: &str,
    data: &Value,
) -> crate::error::Result<Value> {
    let stmt = parse(
        template_str,
        template_name,
        SyntaxConfig,
        WhitespaceConfig::default(),
    )?;

    // Collect used variables from template AST
    let mut root = BTreeMap::new();
    let scope = BTreeMap::new();
    collect_from_stmt(&stmt, &mut root, &scope);

    // Build schema from sample data, but only for variables used in the template
    let mut properties = serde_json::Map::new();
    if let Value::Object(data_map) = data {
        for key in root.keys() {
            if let Some(val) = data_map.get(key) {
                properties.insert(key.clone(), value_to_schema(val));
            }
        }
    }

    let schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "description": format!("Schema for template {}", template_name),
        "properties": Value::Object(properties),
    });

    Ok(schema)
}

/// Convert a JSON value to its corresponding JSON Schema type definition.
fn value_to_schema(val: &Value) -> Value {
    match val {
        Value::String(_) => json!({"type": "string"}),
        Value::Number(_) => json!({"type": "number"}),
        Value::Bool(_) => json!({"type": "boolean"}),
        Value::Null => json!({"type": "null"}),
        Value::Array(arr) => {
            let mut schema = json!({"type": "array"});
            if let Some(first) = arr.first() {
                schema["items"] = value_to_schema(first);
            }
            schema
        }
        Value::Object(obj) => {
            let mut props = serde_json::Map::new();
            for (k, v) in obj {
                props.insert(k.clone(), value_to_schema(v));
            }
            let mut schema = json!({"type": "object"});
            schema["properties"] = Value::Object(props);
            schema
        }
    }
}

#[derive(Debug, Clone)]
enum InferredType {
    String,
    Object(BTreeMap<String, InferredType>),
    Array(Box<InferredType>),
}

/// Convert a BTreeMap of inferred types to a JSON Schema "properties" object.
fn inferred_map_to_schema(map: &BTreeMap<String, InferredType>) -> Value {
    let mut props = serde_json::Map::new();
    for (key, ty) in map {
        props.insert(key.clone(), inferred_to_schema(ty));
    }
    Value::Object(props)
}

fn inferred_to_schema(t: &InferredType) -> Value {
    match t {
        InferredType::String => json!({"type": "string"}),
        InferredType::Object(fields) => {
            let mut schema = json!({"type": "object"});
            schema["properties"] = inferred_map_to_schema(fields);
            schema
        }
        InferredType::Array(inner) => {
            let mut schema = json!({"type": "array"});
            schema["items"] = inferred_to_schema(inner);
            schema
        }
    }
}

/// Scope maps loop variable names to paths in the root context.
/// For example, `{% for item in items %}` maps "item" to ["items"].
type Scope = BTreeMap<String, Vec<String>>;

/// Process a list of statements sequentially, accumulating `set` variable names
/// into the scope so that later statements see them as local variables.
fn collect_from_stmts(
    stmts: &[ast::Stmt<'_>],
    root: &mut BTreeMap<String, InferredType>,
    scope: &Scope,
) {
    let mut scope = scope.clone();
    for stmt in stmts {
        collect_from_stmt(stmt, root, &scope);
        // If this was a Set statement, add the variable to scope for subsequent statements
        if let ast::Stmt::Set(s) = stmt {
            if let ast::Expr::Var(v) = &s.target {
                scope.insert(v.id.to_string(), vec![]);
            }
        }
    }
}

fn collect_from_stmt(
    stmt: &ast::Stmt<'_>,
    root: &mut BTreeMap<String, InferredType>,
    scope: &Scope,
) {
    match stmt {
        ast::Stmt::Template(t) => {
            collect_from_stmts(&t.children, root, scope);
        }
        ast::Stmt::EmitExpr(e) => {
            collect_from_expr(&e.expr, root, scope);
        }
        ast::Stmt::ForLoop(f) => {
            // Extract loop variable name from target
            let loop_var = match &f.target {
                ast::Expr::Var(v) => v.id.to_string(),
                _ => return, // Tuple unpacking etc. not supported yet
            };

            // Resolve the iterator expression to a path
            let iter_path = resolve_expr_path(&f.iter, scope);

            if let Some(path) = &iter_path {
                // Mark the iter path as an Array in root
                ensure_array_at_path(root, path);
            }

            // Create new scope with loop variable mapped
            let mut new_scope = scope.clone();
            if let Some(path) = iter_path {
                new_scope.insert(loop_var, path);
            }

            collect_from_stmts(&f.body, root, &new_scope);
            collect_from_stmts(&f.else_body, root, scope);
        }
        ast::Stmt::IfCond(c) => {
            collect_from_expr(&c.expr, root, scope);
            collect_from_stmts(&c.true_body, root, scope);
            collect_from_stmts(&c.false_body, root, scope);
        }
        ast::Stmt::WithBlock(w) => {
            // Collect from assignment expressions (the right-hand sides)
            for (_, value_expr) in &w.assignments {
                collect_from_expr(value_expr, root, scope);
            }
            // Create a new scope with the `with` variable assignments so they
            // don't leak as top-level schema properties.
            let mut new_scope = scope.clone();
            for (target_expr, _) in &w.assignments {
                if let ast::Expr::Var(v) = target_expr {
                    new_scope.insert(v.id.to_string(), vec![]);
                }
            }
            collect_from_stmts(&w.body, root, &new_scope);
        }
        ast::Stmt::FilterBlock(fb) => {
            collect_from_stmts(&fb.body, root, scope);
        }
        ast::Stmt::AutoEscape(ae) => {
            collect_from_stmts(&ae.body, root, scope);
        }
        ast::Stmt::Set(s) => {
            // Collect from the assigned expression
            collect_from_expr(&s.expr, root, scope);
            // Note: the variable is added to scope by collect_from_stmts
            // for subsequent sibling statements.
        }
        ast::Stmt::SetBlock(sb) => {
            collect_from_stmts(&sb.body, root, scope);
        }
        _ => {}
    }
}

fn collect_from_expr(
    expr: &ast::Expr<'_>,
    root: &mut BTreeMap<String, InferredType>,
    scope: &Scope,
) {
    // Try to resolve expression to a variable path and register it.
    // Skip resolve_path for loop variables used standalone (without attribute access)
    // because their array type is already ensured by ensure_array_at_path in the ForLoop handler.
    // Also skip variables mapped to an empty path (set/with local variables).
    if let Some(path) = resolve_expr_path(expr, scope) {
        let is_scope_var_standalone = matches!(expr, ast::Expr::Var(v) if scope.contains_key(v.id));
        if !is_scope_var_standalone && !path.is_empty() {
            resolve_path(root, &path, InferredType::String);
        }
    }

    // Also recurse into sub-expressions for filters, calls, etc.
    match expr {
        ast::Expr::Filter(f) => {
            if let Some(ref e) = f.expr {
                collect_from_expr(e, root, scope);
            }
            for arg in &f.args {
                collect_from_call_arg(arg, root, scope);
            }
        }
        ast::Expr::Call(c) => {
            collect_from_expr(&c.expr, root, scope);
            for arg in &c.args {
                collect_from_call_arg(arg, root, scope);
            }
        }
        ast::Expr::Test(t) => {
            collect_from_expr(&t.expr, root, scope);
            for arg in &t.args {
                collect_from_call_arg(arg, root, scope);
            }
        }
        ast::Expr::BinOp(b) => {
            collect_from_expr(&b.left, root, scope);
            collect_from_expr(&b.right, root, scope);
        }
        ast::Expr::UnaryOp(u) => {
            collect_from_expr(&u.expr, root, scope);
        }
        ast::Expr::IfExpr(i) => {
            collect_from_expr(&i.test_expr, root, scope);
            collect_from_expr(&i.true_expr, root, scope);
            if let Some(ref fe) = i.false_expr {
                collect_from_expr(fe, root, scope);
            }
        }
        ast::Expr::GetAttr(_) | ast::Expr::Var(_) => {
            // Already handled above via resolve_expr_path
        }
        _ => {}
    }
}

fn collect_from_call_arg(
    arg: &ast::CallArg<'_>,
    root: &mut BTreeMap<String, InferredType>,
    scope: &Scope,
) {
    match arg {
        ast::CallArg::Pos(e)
        | ast::CallArg::Kwarg(_, e)
        | ast::CallArg::PosSplat(e)
        | ast::CallArg::KwargSplat(e) => {
            collect_from_expr(e, root, scope);
        }
    }
}

/// Resolve an expression to a path of variable names.
/// Returns None if the expression is not a simple variable/attribute chain.
fn resolve_expr_path(expr: &ast::Expr<'_>, scope: &Scope) -> Option<Vec<String>> {
    match expr {
        ast::Expr::Var(v) => {
            let name = v.id;
            // Skip built-in variables
            if name == "loop"
                || name == "self"
                || name == "true"
                || name == "false"
                || name == "none"
            {
                return None;
            }
            if let Some(base_path) = scope.get(name) {
                Some(base_path.clone())
            } else {
                Some(vec![name.to_string()])
            }
        }
        ast::Expr::GetAttr(a) => {
            let mut base = resolve_expr_path(&a.expr, scope)?;
            base.push(a.name.to_string());
            Some(base)
        }
        _ => None,
    }
}

/// Merge a path into the root BTreeMap as nested InferredType entries.
/// The leaf is set to `leaf_type`. Intermediate nodes become Object.
fn resolve_path(
    root: &mut BTreeMap<String, InferredType>,
    path: &[String],
    leaf_type: InferredType,
) {
    if path.is_empty() {
        return;
    }

    let key = &path[0];
    if path.len() == 1 {
        // Leaf: only insert if not already present (don't overwrite Object with String)
        root.entry(key.clone()).or_insert(leaf_type);
    } else {
        // Intermediate: ensure this is an Object, then recurse
        let entry = root
            .entry(key.clone())
            .or_insert_with(|| InferredType::Object(BTreeMap::new()));
        match entry {
            InferredType::Object(children) => {
                resolve_path(children, &path[1..], leaf_type);
            }
            InferredType::Array(inner) => {
                // Path continues into array items
                match inner.as_mut() {
                    InferredType::Object(children) => {
                        resolve_path(children, &path[1..], leaf_type);
                    }
                    other => {
                        // Upgrade from String to Object
                        let mut children = BTreeMap::new();
                        resolve_path(&mut children, &path[1..], leaf_type);
                        *other = InferredType::Object(children);
                    }
                }
            }
            _ => {
                // Upgrade String to Object
                let mut children = BTreeMap::new();
                resolve_path(&mut children, &path[1..], leaf_type);
                *entry = InferredType::Object(children);
            }
        }
    }
}

/// Ensure the path points to an Array type in the root map.
fn ensure_array_at_path(root: &mut BTreeMap<String, InferredType>, path: &[String]) {
    if path.is_empty() {
        return;
    }

    let key = &path[0];
    if path.len() == 1 {
        let entry = root
            .entry(key.clone())
            .or_insert_with(|| InferredType::Array(Box::new(InferredType::String)));
        // If it was previously something else, convert to Array
        if !matches!(entry, InferredType::Array(_)) {
            *entry = InferredType::Array(Box::new(InferredType::String));
        }
    } else {
        let entry = root
            .entry(key.clone())
            .or_insert_with(|| InferredType::Object(BTreeMap::new()));
        match entry {
            InferredType::Object(children) => {
                ensure_array_at_path(children, &path[1..]);
            }
            InferredType::Array(inner) => {
                // Path continues into array items
                match inner.as_mut() {
                    InferredType::Object(children) => {
                        ensure_array_at_path(children, &path[1..]);
                    }
                    other => {
                        // Upgrade from String to Object
                        let mut children = BTreeMap::new();
                        ensure_array_at_path(&mut children, &path[1..]);
                        *other = InferredType::Object(children);
                    }
                }
            }
            _ => {
                // Upgrade String to Object
                let mut children = BTreeMap::new();
                ensure_array_at_path(&mut children, &path[1..]);
                *entry = InferredType::Object(children);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_variable() {
        let schema = extract_schema("{{ title }}", "test.html").unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["title"]["type"], "string");
    }

    #[test]
    fn test_for_loop_infers_array() {
        let schema =
            extract_schema("{% for item in items %}{{ item }}{% endfor %}", "test.html").unwrap();
        assert_eq!(schema["properties"]["items"]["type"], "array");
    }

    #[test]
    fn test_nested_attr_infers_object() {
        let schema = extract_schema("{{ user.name }}", "test.html").unwrap();
        assert_eq!(schema["properties"]["user"]["type"], "object");
        assert_eq!(
            schema["properties"]["user"]["properties"]["name"]["type"],
            "string"
        );
    }

    #[test]
    fn test_for_loop_with_attr() {
        let schema = extract_schema(
            "{% for item in items %}{{ item.name }}{% endfor %}",
            "test.html",
        )
        .unwrap();
        let items = &schema["properties"]["items"];
        assert_eq!(items["type"], "array");
        assert_eq!(items["items"]["type"], "object");
        assert_eq!(items["items"]["properties"]["name"]["type"], "string");
    }

    #[test]
    fn test_schema_metadata() {
        let schema = extract_schema("{{ x }}", "invoice.html").unwrap();
        assert_eq!(schema["$schema"], "http://json-schema.org/draft-07/schema#");
        assert!(
            schema["description"]
                .as_str()
                .unwrap()
                .contains("invoice.html")
        );
    }

    #[test]
    fn test_if_condition_variable() {
        let schema = extract_schema("{% if show %}<p>visible</p>{% endif %}", "test.html").unwrap();
        assert_eq!(schema["properties"]["show"]["type"], "string");
    }

    #[test]
    fn test_filter_expression() {
        let schema = extract_schema("{{ name | upper }}", "test.html").unwrap();
        assert_eq!(schema["properties"]["name"]["type"], "string");
    }

    #[test]
    fn test_nested_for_loops() {
        let schema = extract_schema(
            "{% for row in table %}{% for cell in row.cells %}{{ cell.value }}{% endfor %}{% endfor %}",
            "test.html",
        )
        .unwrap();
        let table = &schema["properties"]["table"];
        assert_eq!(table["type"], "array");
        assert_eq!(table["items"]["type"], "object");
        assert_eq!(table["items"]["properties"]["cells"]["type"], "array");
        assert_eq!(
            table["items"]["properties"]["cells"]["items"]["type"],
            "object"
        );
        assert_eq!(
            table["items"]["properties"]["cells"]["items"]["properties"]["value"]["type"],
            "string"
        );
    }

    #[test]
    fn test_with_block_scoping() {
        let schema =
            extract_schema("{% with x = title %}{{ x }}{% endwith %}", "test.html").unwrap();
        // `title` should appear as a top-level property (from the assignment RHS)
        assert_eq!(schema["properties"]["title"]["type"], "string");
        // `x` should NOT appear as a top-level property (it's a local variable)
        assert!(schema["properties"]["x"].is_null());
    }

    #[test]
    fn test_set_variable_scoping() {
        let schema =
            extract_schema("{% set greeting = name %}{{ greeting }}", "test.html").unwrap();
        // `name` should appear (from the set expression)
        assert_eq!(schema["properties"]["name"]["type"], "string");
        // `greeting` should NOT appear (it's a locally-set variable)
        assert!(schema["properties"]["greeting"].is_null());
    }

    #[test]
    fn test_schema_with_sample_data() {
        let data = json!({
            "title": "Invoice",
            "amount": 1234,
            "paid": true,
            "items": [{"name": "Widget", "price": 9.99}]
        });
        let schema = extract_schema_with_data(
            "{{ title }} {{ amount }} {% for i in items %}{{ i.name }}{% endfor %}",
            "test.html",
            &data,
        )
        .unwrap();
        assert_eq!(schema["properties"]["title"]["type"], "string");
        assert_eq!(schema["properties"]["amount"]["type"], "number");
        // "paid" is NOT in the template, so it should NOT be in schema
        assert!(schema["properties"].get("paid").is_none());
        let items = &schema["properties"]["items"];
        assert_eq!(items["type"], "array");
        assert_eq!(items["items"]["properties"]["name"]["type"], "string");
        assert_eq!(items["items"]["properties"]["price"]["type"], "number");
    }

    #[test]
    fn test_data_only_exports_used_variables() {
        let data = json!({"used": "yes", "unused": "no"});
        let schema = extract_schema_with_data("{{ used }}", "test.html", &data).unwrap();
        assert!(schema["properties"].get("used").is_some());
        assert!(schema["properties"].get("unused").is_none());
    }
}

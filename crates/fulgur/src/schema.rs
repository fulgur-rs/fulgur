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
    let mut scope = BTreeMap::new();
    collect_from_stmt(&stmt, &mut root, &mut scope);

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
    let mut scope = BTreeMap::new();
    collect_from_stmt(&stmt, &mut root, &mut scope);

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

/// Extract all variable names from a target expression (supports tuple unpacking).
/// For example, `(key, value)` yields `["key", "value"]`.
fn extract_var_names(expr: &ast::Expr<'_>) -> Vec<String> {
    match expr {
        ast::Expr::Var(v) => vec![v.id.to_string()],
        ast::Expr::List(l) => l.items.iter().flat_map(extract_var_names).collect(),
        _ => vec![],
    }
}

/// Accumulate `set`/`setblock` variable bindings from a statement into the scope.
fn accumulate_set_in_scope(stmt: &ast::Stmt<'_>, scope: &mut Scope) {
    if let ast::Stmt::Set(s) = stmt {
        if let ast::Expr::Var(v) = &s.target {
            let rhs_path = resolve_expr_path(&s.expr, scope).unwrap_or_default();
            scope.insert(v.id.to_string(), rhs_path);
        }
    }
    if let ast::Stmt::SetBlock(sb) = stmt {
        if let ast::Expr::Var(v) = &sb.target {
            scope.insert(v.id.to_string(), vec![]);
        }
    }
}

/// Process an IfCond statement, returning the independent scopes for each branch.
fn collect_if_cond_scopes(
    stmt: &ast::Stmt<'_>,
    root: &mut BTreeMap<String, InferredType>,
    scope: &Scope,
) -> (Scope, Scope) {
    if let ast::Stmt::IfCond(c) = stmt {
        collect_from_expr(&c.expr, root, scope);
        let mut true_scope = scope.clone();
        collect_from_stmts(&c.true_body, root, &mut true_scope);
        let mut false_scope = scope.clone();
        collect_from_stmts(&c.false_body, root, &mut false_scope);
        (true_scope, false_scope)
    } else {
        (scope.clone(), scope.clone())
    }
}

/// Process a list of statements sequentially, accumulating `set` variable names
/// into the scope so that later statements see them as local variables.
/// The scope is mutated in place; callers that need a new scope should clone before calling.
fn collect_from_stmts(
    stmts: &[ast::Stmt<'_>],
    root: &mut BTreeMap<String, InferredType>,
    scope: &mut Scope,
) {
    for (i, stmt) in stmts.iter().enumerate() {
        if let ast::Stmt::IfCond(_) = stmt {
            // Process IfCond and get the divergent scopes
            let (true_scope, false_scope) = collect_if_cond_scopes(stmt, root, scope);
            // Process remaining statements with each branch's scope independently
            let remaining = &stmts[i + 1..];
            if !remaining.is_empty() {
                let mut true_s = true_scope;
                collect_from_stmts(remaining, root, &mut true_s);
                let mut false_s = false_scope;
                collect_from_stmts(remaining, root, &mut false_s);
                // Merge both scopes into parent
                for (k, v) in true_s {
                    scope.entry(k).or_insert(v);
                }
                for (k, v) in false_s {
                    scope.entry(k).or_insert(v);
                }
            } else {
                // No remaining statements, just merge scopes
                for (k, v) in true_scope {
                    scope.entry(k).or_insert(v);
                }
                for (k, v) in false_scope {
                    scope.entry(k).or_insert(v);
                }
            }
            return; // remaining already processed
        }
        collect_from_stmt(stmt, root, scope);
        accumulate_set_in_scope(stmt, scope);
    }
}

fn collect_from_stmt(
    stmt: &ast::Stmt<'_>,
    root: &mut BTreeMap<String, InferredType>,
    scope: &mut Scope,
) {
    match stmt {
        ast::Stmt::Template(t) => {
            collect_from_stmts(&t.children, root, scope);
        }
        ast::Stmt::EmitExpr(e) => {
            collect_from_expr(&e.expr, root, scope);
        }
        ast::Stmt::ForLoop(f) => {
            // Extract all variable names from the target (supports tuple unpacking)
            let var_names = extract_var_names(&f.target);

            // Resolve the iterator expression to a path
            let iter_path = resolve_expr_path(&f.iter, scope);

            if let Some(path) = &iter_path {
                // Mark the iter path as an Array in root
                ensure_array_at_path(root, path);
            }

            // Collect variables from the iter expression itself
            collect_from_expr(&f.iter, root, scope);

            // Create new scope with loop variable(s) mapped
            let mut body_scope = scope.clone();
            if let Some(path) = &iter_path {
                if var_names.len() == 1 {
                    // Single variable: map to iter path for attribute resolution
                    body_scope.insert(var_names[0].clone(), path.clone());
                } else {
                    // Tuple unpacking: variables are local (empty path)
                    for name in &var_names {
                        body_scope.insert(name.clone(), vec![]);
                    }
                }
            } else {
                // No resolvable iter path: all vars are local
                for name in &var_names {
                    body_scope.insert(name.clone(), vec![]);
                }
            }

            collect_from_stmts(&f.body, root, &mut body_scope);
            let mut else_scope = scope.clone();
            collect_from_stmts(&f.else_body, root, &mut else_scope);
        }
        ast::Stmt::IfCond(_) => {
            // IfCond is handled specially by collect_from_stmts to process
            // remaining sibling statements with each branch's scope independently.
            // When called directly (e.g. from Template), use the helper.
            let (true_scope, false_scope) = collect_if_cond_scopes(stmt, root, scope);
            for (k, v) in true_scope {
                scope.entry(k).or_insert(v);
            }
            for (k, v) in false_scope {
                scope.entry(k).or_insert(v);
            }
        }
        ast::Stmt::WithBlock(w) => {
            // Collect from assignment expressions (the right-hand sides)
            for (_, value_expr) in &w.assignments {
                collect_from_expr(value_expr, root, scope);
            }
            // Create a new scope with the `with` variable assignments so they
            // don't leak as top-level schema properties.
            let mut new_scope = scope.clone();
            for (target_expr, value_expr) in &w.assignments {
                let var_names = extract_var_names(target_expr);
                if var_names.len() == 1 {
                    let rhs_path = resolve_expr_path(value_expr, &new_scope).unwrap_or_default();
                    new_scope.insert(var_names[0].clone(), rhs_path);
                } else {
                    // Tuple unpacking: all vars are local
                    for name in var_names {
                        new_scope.insert(name, vec![]);
                    }
                }
            }
            collect_from_stmts(&w.body, root, &mut new_scope);
        }
        ast::Stmt::FilterBlock(fb) => {
            let mut body_scope = scope.clone();
            collect_from_stmts(&fb.body, root, &mut body_scope);
        }
        ast::Stmt::AutoEscape(ae) => {
            let mut body_scope = scope.clone();
            collect_from_stmts(&ae.body, root, &mut body_scope);
        }
        ast::Stmt::Set(s) => {
            // Collect from the assigned expression
            collect_from_expr(&s.expr, root, scope);
            // Note: the variable is added to scope by collect_from_stmts
            // for subsequent sibling statements.
        }
        ast::Stmt::SetBlock(sb) => {
            let mut body_scope = scope.clone();
            collect_from_stmts(&sb.body, root, &mut body_scope);
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
        ast::Expr::GetAttr(a)
            // If resolve_expr_path returned None (e.g. filter/call base),
            // recurse into the base expression to collect its variables.
            if resolve_expr_path(expr, scope).is_none() =>
        {
            collect_from_expr(&a.expr, root, scope);
        }
        ast::Expr::GetItem(gi) => {
            // Recurse into both base and subscript expressions
            collect_from_expr(&gi.expr, root, scope);
            collect_from_expr(&gi.subscript_expr, root, scope);
        }
        ast::Expr::List(l) => {
            for item in &l.items {
                collect_from_expr(item, root, scope);
            }
        }
        ast::Expr::Map(m) => {
            for k in &m.keys {
                collect_from_expr(k, root, scope);
            }
            for v in &m.values {
                collect_from_expr(v, root, scope);
            }
        }
        ast::Expr::Var(_) => {
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
                if base_path.is_empty() {
                    None // Local variable (e.g. setblock, namespace), not an input
                } else {
                    Some(base_path.clone())
                }
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

    #[test]
    fn test_set_with_attr_access() {
        let schema = extract_schema("{% set u = user %}{{ u.name }}", "test.html").unwrap();
        assert_eq!(schema["properties"]["user"]["type"], "object");
        assert_eq!(
            schema["properties"]["user"]["properties"]["name"]["type"],
            "string"
        );
        assert!(schema["properties"]["u"].is_null());
    }

    #[test]
    fn test_for_loop_tuple_unpacking_still_collects_body() {
        let schema = extract_schema(
            "{% for key, value in pairs %}{{ title }}{% endfor %}",
            "test.html",
        )
        .unwrap();
        // title should be in schema even though tuple unpacking is not supported
        assert_eq!(schema["properties"]["title"]["type"], "string");
        // pairs should be an array (from the iterator)
        assert_eq!(schema["properties"]["pairs"]["type"], "array");
    }

    #[test]
    fn test_for_loop_tuple_unpacking_no_leak() {
        let schema = extract_schema(
            "{% for key, value in pairs %}{{ key }}: {{ value }}{% endfor %}",
            "test.html",
        )
        .unwrap();
        // pairs should be an array
        assert_eq!(schema["properties"]["pairs"]["type"], "array");
        // key and value are loop-local — they must NOT leak to top-level schema
        assert!(schema["properties"]["key"].is_null());
        assert!(schema["properties"]["value"].is_null());
    }

    #[test]
    fn test_if_set_both_branches() {
        // When both branches set the same variable, both RHS paths are resolved
        let schema = extract_schema(
            "{% if cond %}{% set u = user %}{% else %}{% set u = admin %}{% endif %}{{ u.name }}",
            "test.html",
        )
        .unwrap();
        assert_eq!(schema["properties"]["user"]["type"], "object");
        assert_eq!(
            schema["properties"]["user"]["properties"]["name"]["type"],
            "string"
        );
        assert_eq!(schema["properties"]["admin"]["type"], "object");
        assert_eq!(
            schema["properties"]["admin"]["properties"]["name"]["type"],
            "string"
        );
    }

    #[test]
    fn test_getitem_collects_variables() {
        let schema = extract_schema("{{ items[key] }}", "test.html").unwrap();
        // Both items and key should be collected as top-level variables
        assert_eq!(schema["properties"]["items"]["type"], "string");
        assert_eq!(schema["properties"]["key"]["type"], "string");
    }

    #[test]
    fn test_namespace_local_does_not_leak() {
        let schema = extract_schema(
            "{% set ns = namespace(found=false) %}{{ ns.found }}",
            "test.html",
        )
        .unwrap();
        // ns is a local (namespace() call can't resolve to a path),
        // ns.found should NOT appear as top-level "found"
        assert!(schema["properties"].get("found").is_none());
    }

    #[test]
    fn test_if_branches_independent_scope() {
        let schema = extract_schema(
            "{% if cond %}{% set x = user %}{% else %}{% set x = account %}{% endif %}{{ x.id }}",
            "test.html",
        )
        .unwrap();
        // Both user and account should be collected (conservative merge)
        assert_eq!(schema["properties"]["user"]["type"], "object");
        assert_eq!(
            schema["properties"]["user"]["properties"]["id"]["type"],
            "string"
        );
        assert_eq!(schema["properties"]["account"]["type"], "object");
        assert_eq!(
            schema["properties"]["account"]["properties"]["id"]["type"],
            "string"
        );
        assert!(schema["properties"]["x"].is_null());
    }

    #[test]
    fn test_list_expression_collects_variables() {
        let schema = extract_schema("{{ [title, subtitle] | join(', ') }}", "test.html").unwrap();
        assert_eq!(schema["properties"]["title"]["type"], "string");
        assert_eq!(schema["properties"]["subtitle"]["type"], "string");
    }

    // ── FilterBlock ──────────────────────────────────────────────────────────

    #[test]
    fn filter_block_collects_body_variables() {
        let schema =
            extract_schema("{% filter upper %}{{ name }}{% endfilter %}", "test.html").unwrap();
        assert_eq!(schema["properties"]["name"]["type"], "string");
    }

    // ── AutoEscape ───────────────────────────────────────────────────────────

    #[test]
    fn autoescape_block_collects_body_variables() {
        let schema = extract_schema(
            r#"{% autoescape true %}{{ name }}{% endautoescape %}"#,
            "test.html",
        )
        .unwrap();
        assert_eq!(schema["properties"]["name"]["type"], "string");
    }

    // ── SetBlock ─────────────────────────────────────────────────────────────

    #[test]
    fn setblock_body_is_walked_and_target_does_not_leak() {
        // {% set x %}...{% endset %} walks the block body but x itself
        // is a locally-captured string and must not appear as a top-level
        // schema property.
        let schema =
            extract_schema("{% set content %}{{ source_var }}{% endset %}", "test.html").unwrap();
        assert_eq!(schema["properties"]["source_var"]["type"], "string");
        assert!(schema["properties"]["content"].is_null());
    }

    // ── UnaryOp ──────────────────────────────────────────────────────────────

    #[test]
    fn unary_op_collects_operand_variable() {
        let schema = extract_schema("{{ not flag }}", "test.html").unwrap();
        assert_eq!(schema["properties"]["flag"]["type"], "string");
    }

    // ── IfExpr (inline ternary) ───────────────────────────────────────────────

    #[test]
    fn inline_if_with_else_collects_all_three_parts() {
        let schema = extract_schema("{{ title if show else fallback }}", "test.html").unwrap();
        assert_eq!(schema["properties"]["title"]["type"], "string");
        assert_eq!(schema["properties"]["show"]["type"], "string");
        assert_eq!(schema["properties"]["fallback"]["type"], "string");
    }

    #[test]
    fn inline_if_without_else_collects_value_and_test() {
        let schema = extract_schema("{{ title if show }}", "test.html").unwrap();
        assert_eq!(schema["properties"]["title"]["type"], "string");
        assert_eq!(schema["properties"]["show"]["type"], "string");
    }

    // ── Test expression (is defined / is …) ──────────────────────────────────

    #[test]
    fn test_expression_collects_subject_variable() {
        let schema = extract_schema("{{ value is defined }}", "test.html").unwrap();
        assert_eq!(schema["properties"]["value"]["type"], "string");
    }

    // ── BinOp ────────────────────────────────────────────────────────────────

    #[test]
    fn bin_op_collects_both_operands() {
        let schema = extract_schema("{{ first ~ last }}", "test.html").unwrap();
        assert_eq!(schema["properties"]["first"]["type"], "string");
        assert_eq!(schema["properties"]["last"]["type"], "string");
    }

    // ── Map literal ──────────────────────────────────────────────────────────

    #[test]
    fn map_literal_collects_value_variables() {
        let schema =
            extract_schema(r#"{{ {"label": title, "id": page_id} }}"#, "test.html").unwrap();
        assert_eq!(schema["properties"]["title"]["type"], "string");
        assert_eq!(schema["properties"]["page_id"]["type"], "string");
    }

    // ── value_to_schema edge cases ────────────────────────────────────────────

    #[test]
    fn schema_with_data_boolean_maps_to_boolean_type() {
        let data = json!({"active": true});
        let schema = extract_schema_with_data("{{ active }}", "test.html", &data).unwrap();
        assert_eq!(schema["properties"]["active"]["type"], "boolean");
    }

    #[test]
    fn schema_with_data_null_maps_to_null_type() {
        let data = json!({"val": null});
        let schema = extract_schema_with_data("{{ val }}", "test.html", &data).unwrap();
        assert_eq!(schema["properties"]["val"]["type"], "null");
    }

    #[test]
    fn schema_with_data_object_includes_all_nested_properties() {
        let data = json!({
            "user": {"name": "Alice", "age": 30, "active": true}
        });
        let schema = extract_schema_with_data("{{ user.name }}", "test.html", &data).unwrap();
        let user = &schema["properties"]["user"];
        assert_eq!(user["type"], "object");
        assert_eq!(user["properties"]["name"]["type"], "string");
        assert_eq!(user["properties"]["age"]["type"], "number");
        assert_eq!(user["properties"]["active"]["type"], "boolean");
    }

    #[test]
    fn schema_with_data_empty_array_omits_items_schema() {
        // When the sample array is empty, value_to_schema cannot infer an
        // items schema, so the "items" key must be absent.
        let data = json!({"tags": []});
        let schema =
            extract_schema_with_data("{% for t in tags %}{{ t }}{% endfor %}", "test.html", &data)
                .unwrap();
        let tags = &schema["properties"]["tags"];
        assert_eq!(tags["type"], "array");
        assert!(tags.get("items").is_none() || tags["items"].is_null());
    }

    // ── Parse error propagation ───────────────────────────────────────────────

    #[test]
    fn extract_schema_returns_error_on_invalid_syntax() {
        let result = extract_schema("{{ unclosed", "test.html");
        assert!(result.is_err());
    }

    #[test]
    fn extract_schema_with_data_returns_error_on_invalid_syntax() {
        let result = extract_schema_with_data("{{ unclosed", "test.html", &json!({}));
        assert!(result.is_err());
    }

    // ── Non-Object data in extract_schema_with_data ───────────────────────────

    #[test]
    fn schema_with_data_non_object_data_yields_empty_properties() {
        // data is an array, not an object — no variables can be matched
        let schema =
            extract_schema_with_data("{{ title }}", "test.html", &json!([1, 2, 3])).unwrap();
        assert_eq!(schema["properties"], json!({}));
    }

    // ── IfCond as the last sibling (no remaining statements) ──────────────────

    #[test]
    fn if_cond_as_last_stmt_merges_both_branch_scopes_back() {
        // IfCond is the last child, so the "no remaining" else path in
        // collect_from_stmts is taken and both branch scopes are merged back.
        let schema = extract_schema(
            "{% set x = val %}{% if cond %}{% set x = user %}{% endif %}",
            "test.html",
        )
        .unwrap();
        assert_eq!(schema["properties"]["val"]["type"], "string");
        assert_eq!(schema["properties"]["user"]["type"], "string");
        assert_eq!(schema["properties"]["cond"]["type"], "string");
        // x is a local set variable — must not appear in schema
        assert!(schema["properties"]["x"].is_null());
    }

    // ── ForLoop with non-resolvable iterator (literal list) ───────────────────

    #[test]
    fn for_loop_over_literal_list_treats_loop_var_as_local() {
        // The iterator `[1, 2, 3]` is a List expression, so iter_path is None;
        // `item` becomes a local variable with empty path and must not leak.
        let schema = extract_schema(
            "{% for item in [1, 2, 3] %}{{ title }}{% endfor %}",
            "test.html",
        )
        .unwrap();
        assert_eq!(schema["properties"]["title"]["type"], "string");
        assert!(schema["properties"]["item"].is_null());
    }

    // ── ForLoop else body ─────────────────────────────────────────────────────

    #[test]
    fn for_loop_else_body_collects_its_variables() {
        let schema = extract_schema(
            "{% for item in items %}{{ item }}{% else %}{{ empty_msg }}{% endfor %}",
            "test.html",
        )
        .unwrap();
        assert_eq!(schema["properties"]["items"]["type"], "array");
        assert_eq!(schema["properties"]["empty_msg"]["type"], "string");
    }

    // ── Built-in `loop` variable is not collected ─────────────────────────────

    #[test]
    fn loop_builtin_not_collected_as_schema_variable() {
        let schema = extract_schema(
            "{% for item in items %}{{ loop.index }}{% endfor %}",
            "test.html",
        )
        .unwrap();
        // `loop` is a Jinja built-in — must NOT appear in the inferred schema
        assert!(schema["properties"]["loop"].is_null());
        assert_eq!(schema["properties"]["items"]["type"], "array");
    }

    // ── ensure_array_at_path: non-Array entry converted to Array ──────────────

    #[test]
    fn var_used_as_scalar_then_as_iter_is_converted_to_array() {
        // {{ items }} registers `items` as String; {% for x in items %} must
        // upgrade it to Array via the conversion branch in ensure_array_at_path.
        let schema = extract_schema(
            "{{ items }}{% for x in items %}{{ x }}{% endfor %}",
            "test.html",
        )
        .unwrap();
        assert_eq!(schema["properties"]["items"]["type"], "array");
    }

    // ── ensure_array_at_path: nested path through Object intermediate ─────────

    #[test]
    fn nested_iter_path_through_object_creates_array_inside_object() {
        // `table.rows` forces ensure_array_at_path to create Object("table")
        // first, then Array("rows") inside it.
        let schema = extract_schema(
            "{% for row in table.rows %}{{ row.value }}{% endfor %}",
            "test.html",
        )
        .unwrap();
        assert_eq!(schema["properties"]["table"]["type"], "object");
        let rows = &schema["properties"]["table"]["properties"]["rows"];
        assert_eq!(rows["type"], "array");
        assert_eq!(rows["items"]["type"], "object");
        assert_eq!(rows["items"]["properties"]["value"]["type"], "string");
    }

    // ── ensure_array_at_path: String entry upgraded to Object then Array ───────

    #[test]
    fn plain_var_then_nested_iter_upgrades_to_object_with_inner_array() {
        // {{ data }} registers `data` as String; {% for item in data.rows %}
        // must upgrade `data` to Object and `rows` to Array via the `_` arm.
        let schema = extract_schema(
            "{{ data }}{% for item in data.rows %}{{ item.id }}{% endfor %}",
            "test.html",
        )
        .unwrap();
        assert_eq!(schema["properties"]["data"]["type"], "object");
        let rows = &schema["properties"]["data"]["properties"]["rows"];
        assert_eq!(rows["type"], "array");
        assert_eq!(rows["items"]["properties"]["id"]["type"], "string");
    }

    // ── ensure_array_at_path: Array(Object) arm ───────────────────────────────

    #[test]
    fn attr_access_followed_by_nested_iter_on_same_var_uses_object_arm() {
        // After item.name upgrades items to Array(Object), the inner for-loop
        // on item.sub_items must go through the Array(Object) arm in
        // ensure_array_at_path to add sub_items inside the object.
        let schema = extract_schema(
            "{% for item in items %}{{ item.name }}{% for sub in item.sub_items %}{{ sub }}{% endfor %}{% endfor %}",
            "test.html",
        )
        .unwrap();
        let items = &schema["properties"]["items"];
        assert_eq!(items["type"], "array");
        assert_eq!(items["items"]["type"], "object");
        assert_eq!(items["items"]["properties"]["name"]["type"], "string");
        assert_eq!(items["items"]["properties"]["sub_items"]["type"], "array");
    }

    // ── resolve_path: multiple attrs on same loop var merge into one object ─────

    #[test]
    fn multiple_attrs_on_loop_var_are_merged_into_single_object_schema() {
        // item.name creates Array(String) → Array(Object(name)); item.age then
        // goes through the Array(Object) arm to add age without resetting.
        let schema = extract_schema(
            "{% for item in items %}{{ item.name }}{{ item.age }}{% endfor %}",
            "test.html",
        )
        .unwrap();
        let items = &schema["properties"]["items"];
        assert_eq!(items["type"], "array");
        assert_eq!(items["items"]["type"], "object");
        assert_eq!(items["items"]["properties"]["name"]["type"], "string");
        assert_eq!(items["items"]["properties"]["age"]["type"], "string");
    }

    // ── resolve_path: String entry upgraded to Object ─────────────────────────

    #[test]
    fn plain_var_then_attr_access_upgrades_type_to_object() {
        // {{ user }} registers user as String; {{ user.name }} must upgrade
        // it to Object via the `_` arm in resolve_path.
        let schema = extract_schema("{{ user }}{{ user.name }}", "test.html").unwrap();
        assert_eq!(schema["properties"]["user"]["type"], "object");
        assert_eq!(
            schema["properties"]["user"]["properties"]["name"]["type"],
            "string"
        );
    }

    // ── Test expression with positional arg collects the arg variable ──────────

    #[test]
    fn test_expr_with_variable_arg_collects_arg_as_schema_property() {
        // `value is divisibleby(divisor)` has a Test arg; divisor must be
        // collected even though it appears as a test argument, not a direct
        // template output.
        let schema = extract_schema("{{ value is divisibleby(divisor) }}", "test.html").unwrap();
        assert_eq!(schema["properties"]["value"]["type"], "string");
        assert_eq!(schema["properties"]["divisor"]["type"], "string");
    }

    // ── collect_from_stmt catch-all `_ => {}` (Include, Extends, etc.) ───────

    /// `{% include %}` hits the catch-all arm in `collect_from_stmt`.
    /// Variables in surrounding statements must still be collected.
    #[test]
    fn include_stmt_does_not_discard_surrounding_variables() {
        let schema = extract_schema(
            r#"{{ header_title }}{% include "header.html" %}{{ page_body }}"#,
            "test.html",
        )
        .unwrap();
        assert_eq!(schema["properties"]["header_title"]["type"], "string");
        assert_eq!(schema["properties"]["page_body"]["type"], "string");
        // The include target name is a constant, not a variable.
        assert!(schema["properties"].get("header_title").is_some());
    }

    /// Multiple ignorable statement types (include, extends) all land in the
    /// catch-all arm and leave surrounding variable collection intact.
    #[test]
    fn multiple_ignored_stmts_do_not_break_collection() {
        let schema = extract_schema(
            r#"{% include "head.html" %}{{ page_title }}{% include "foot.html" %}"#,
            "test.html",
        )
        .unwrap();
        assert_eq!(schema["properties"]["page_title"]["type"], "string");
    }

    // ── collect_from_expr: guarded GetAttr arm (filter output attribute) ─────

    /// `(items | first).name`: the base of the GetAttr is a Filter, so
    /// `resolve_expr_path` returns `None` for the outer GetAttr, triggering
    /// the guarded arm that recurses into the base expression.
    #[test]
    fn getattr_on_filter_output_collects_base_variable() {
        let schema = extract_schema("{{ (items | first).name }}", "test.html").unwrap();
        // `items` must be collected — it is the base of the filter.
        assert!(!schema["properties"]["items"].is_null());
    }

    /// `(call_result()).attr` — call result attribute access also makes
    /// `resolve_expr_path` return `None` for the outer GetAttr.
    #[test]
    fn getattr_on_call_result_collects_call_base_variable() {
        // `get_item()` is a function call; the outer `.title` triggers the
        // guarded GetAttr arm which recurses into the call expression.
        let schema = extract_schema("{{ get_item().title }}", "test.html").unwrap();
        // `get_item` is a Var inside the Call; it should be collected.
        assert!(!schema["properties"]["get_item"].is_null());
    }

    // ── resolve_expr_path: built-in `self` variable name ────────────────────

    /// `self` is a Jinja built-in; `resolve_expr_path` skips it by name so it
    /// never appears as a top-level schema property.
    #[test]
    fn self_builtin_is_not_collected_as_schema_variable() {
        let schema = extract_schema("{{ self.title }}", "test.html").unwrap();
        assert!(
            schema["properties"]["self"].is_null(),
            "`self` must not appear as a schema property"
        );
    }

    // ── extract_schema_with_data: template variable absent from data ─────────

    /// A template variable that is NOT present in the sample data object must
    /// be omitted from the generated schema.  This exercises the
    /// `if let Some(val) = data_map.get(key)` branch that returns `None`.
    #[test]
    fn schema_with_data_absent_template_var_is_omitted() {
        // data has only "title"; the template also uses "author".
        // For "author", data_map.get("author") returns None → that branch fires.
        let data = json!({"title": "My Page"});
        let schema =
            extract_schema_with_data("{{ title }} by {{ author }}", "test.html", &data).unwrap();
        assert_eq!(
            schema["properties"],
            json!({"title": {"type": "string"}}),
            "`author` (absent from data) must not appear in the schema"
        );
    }
}

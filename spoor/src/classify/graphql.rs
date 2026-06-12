use graphql_parser::query::{Definition, OperationDefinition};
use graphql_parser::parse_query;

use crate::ir::TrafficEntry;

pub fn try_parse_operation(entry: &TrafficEntry) -> Option<String> {
    if !matches!(entry.flow.method.to_uppercase().as_str(), "POST" | "GET") {
        return None;
    }
    if let Some(name) = try_parse_aem_execute_path(&entry.path) {
        return Some(name);
    }
    let body = entry.flow.request_body.as_ref()?;
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let query = json.get("query").and_then(|q| q.as_str())?;
    if query.trim().is_empty() {
        return None;
    }

    if let Some(name) = json
        .get("operationName")
        .and_then(|n| n.as_str())
        .filter(|n| !n.is_empty())
    {
        if parse_query::<&str>(query).is_ok() {
            return Some(name.to_string());
        }
        return None;
    }

    let doc = parse_query::<&str>(query).ok()?;
    let mut names = Vec::new();
    for def in &doc.definitions {
        if let Definition::Operation(op) = def {
            if let Some(n) = operation_name(op) {
                names.push(n);
            }
        }
    }
    if names.len() == 1 {
        return Some(names.pop()?);
    }
    if names.is_empty() {
        return Some(anonymous_label(query));
    }
    None
}

fn operation_name<'a>(op: &OperationDefinition<'a, &'a str>) -> Option<String> {
    match op {
        OperationDefinition::SelectionSet(_) => None,
        OperationDefinition::Query(q) => q.name.map(|n| n.to_string()),
        OperationDefinition::Mutation(m) => m.name.map(|n| n.to_string()),
        OperationDefinition::Subscription(s) => s.name.map(|n| n.to_string()),
    }
}

/// AEM-style: `/graphql/execute.json/.../onboarding_teaser%3Blocale%3Dde`
fn try_parse_aem_execute_path(path: &str) -> Option<String> {
    let marker = "/graphql/execute.json/";
    let rest = path.split_once(marker).map(|(_, r)| r)?;
    let segment = rest.rsplit('/').next()?;
    let decoded = segment.replace("%3B", ";").replace("%3b", ";");
    let op = decoded.split(';').next()?.trim();
    if op.is_empty() {
        None
    } else {
        Some(op.to_string())
    }
}

fn anonymous_label(query: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    query.trim().hash(&mut hasher);
    format!("anonymous_{:x}", hasher.finish())
}

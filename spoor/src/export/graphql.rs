use std::collections::BTreeMap;

use serde::Serialize;

use crate::classify::{ClassifiedEntry, Protocol};

#[derive(Serialize)]
struct GraphqlOpsFile {
    origin: String,
    operations: Vec<GraphqlOperation>,
}

#[derive(Serialize)]
struct GraphqlOperation {
    name: String,
    method: String,
    url: String,
    example_request: Option<serde_json::Value>,
    example_response: Option<serde_json::Value>,
}

pub fn generate_operations_yaml(
    classified: &[ClassifiedEntry],
    origin: &str,
    operation_names: &[String],
) -> anyhow::Result<String> {
    let selected_ops: std::collections::HashSet<String> =
        operation_names.iter().cloned().collect();

    let mut by_op: BTreeMap<String, ClassifiedEntry> = BTreeMap::new();
    for item in classified.iter().filter(|c| c.protocol == Protocol::Graphql && c.entry.origin == origin)
    {
        let name = item
            .operation_name
            .clone()
            .unwrap_or_else(|| "anonymous".to_string());
        if !selected_ops.is_empty() && !selected_ops.contains(&name) {
            continue;
        }
        by_op.entry(name).or_insert_with(|| item.clone());
    }

    let operations: Vec<GraphqlOperation> = by_op
        .into_iter()
        .map(|(name, item)| {
            let req_json = item
                .entry
                .flow
                .request_body
                .as_ref()
                .and_then(|b| serde_json::from_str(b).ok());
            let resp_json = item
                .entry
                .flow
                .response_body
                .as_ref()
                .and_then(|b| serde_json::from_str(b).ok());
            GraphqlOperation {
                name,
                method: item.entry.flow.method.to_uppercase(),
                url: item.entry.flow.url.clone(),
                example_request: req_json,
                example_response: resp_json,
            }
        })
        .collect();

    let file = GraphqlOpsFile {
        origin: origin.to_string(),
        operations,
    };

    Ok(serde_yaml_ng::to_string(&file)?)
}

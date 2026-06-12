use crate::classify::{ClassifiedEntry, Protocol};
use crate::ir::MemoryRequest;
use crate::rest::{spec_to_yaml, Config, OpenApiBuilder};

pub fn generate_openapi(
    classified: &[ClassifiedEntry],
    origin: &str,
    templates: &[String],
) -> anyhow::Result<String> {
    let prefix = origin.trim_end_matches('/').to_string();

    let config = Config {
        prefix: prefix.clone(),
        openapi_version: "1.0.0".to_string(),
        skip_options: true,
        max_examples: 3,
        ..Config::default()
    };

    let mut builder = OpenApiBuilder::new(&prefix, &config, templates.to_vec());

    for item in classified.iter().filter(|c| c.protocol == Protocol::Rest && c.entry.origin == origin)
    {
        let req = MemoryRequest::from_entry(&item.entry);
        builder.add_request(&req);
    }

    let spec = builder.build();
    Ok(spec_to_yaml(&spec)?)
}

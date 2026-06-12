// Portions derived from mitm2openapi (MIT, Arkptz 2026).
// See THIRD_PARTY_NOTICES.md.

#![allow(clippy::indexing_slicing)]
#![allow(dead_code)]

pub mod builder;
pub mod envelope;
pub mod error;
pub mod har_reader;
pub mod operation_id;
pub mod output;
pub mod params;
pub mod path_matching;
pub mod redact;
pub mod schema;
pub mod tag_rules;
pub mod type_hints;
pub mod types;

pub const MAX_SCHEMA_DEPTH: usize = 64;
pub const MIN_VARIABILITY_CARDINALITY: usize = 3;
pub const MAX_BODY_SIZE: usize = 64 * 1024 * 1024;

pub use builder::OpenApiBuilder;
pub use output::spec_to_yaml;
pub use types::Config;

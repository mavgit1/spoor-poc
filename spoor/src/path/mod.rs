//! REST path templating for discover (parameterized segments from captured paths).

mod matching;
mod segments;

pub use matching::{is_param_segment, path_matches_template, suggest_param_templates};

pub const MIN_VARIABILITY_CARDINALITY: usize = 3;

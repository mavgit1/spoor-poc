//! Delegates to unified [`crate::classify::filters::is_static_asset_path`].

pub fn is_static_asset_path(path: &str) -> bool {
    crate::classify::filters::is_static_asset_path(path)
}

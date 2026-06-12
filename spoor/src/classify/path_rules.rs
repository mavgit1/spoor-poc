/// Generic path patterns for static assets — not host-specific (see PLAN path_rules).

pub fn is_static_asset_path(path: &str) -> bool {
    let p = path.to_lowercase();
    p.starts_with("/_next/static/")
        || p.ends_with(".css")
        || p.ends_with(".js")
        || p.ends_with(".map")
        || p.ends_with(".woff2")
        || p.ends_with(".woff")
        || p.ends_with(".svg")
        || p.ends_with(".png")
        || p.ends_with(".ico")
        || p.contains("/icons/")
        || p.contains("/fonts/")
        || p.contains("/logos/")
        || p.contains("/scripttemplates/")
}

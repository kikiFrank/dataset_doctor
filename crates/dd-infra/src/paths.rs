use std::path::{Component, Path};

pub(crate) fn is_portable_relative_file(path: &Path) -> bool {
    path.file_name().is_some()
        && path.components().all(|component| {
            matches!(component, Component::Normal(name) if name.to_str().is_some_and(|name| {
                !name.contains([':', '\\']) && !name.ends_with(['.', ' '])
            }))
        })
}

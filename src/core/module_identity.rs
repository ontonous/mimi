//! Stable conversion and matching for loader-owned source identities.
//!
//! The checker uses these helpers only to determine whether a declaration
//! originated in an explicitly imported module. They deliberately avoid
//! basename matching so same-named files in separate directories cannot
//! satisfy the same import accidentally.

use crate::span::SourceKey;

pub(crate) fn module_path_from_source_key(key: &SourceKey) -> Option<String> {
    let value = key.as_str();
    let (namespace, rest) = value.split_once(':')?;
    match namespace {
        "stdlib" => Some(format!(
            "std::{}",
            without_mimi_suffix(rest).replace('/', "::")
        )),
        "workspace" => Some(without_mimi_suffix(rest).replace('/', "::")),
        "package" => {
            let (package, relative) = rest.split_once(':')?;
            let package = package.split('@').next().unwrap_or(package);
            Some(format!(
                "{}::{}",
                package,
                without_mimi_suffix(relative).replace('/', "::")
            ))
        }
        _ => None,
    }
}

pub(crate) fn module_path_matches_import_path(module_path: &str, import_path: &str) -> bool {
    let module = module_path
        .trim_matches(':')
        .split("::")
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let import = import_path
        .trim_matches(':')
        .split("::")
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if module.is_empty() || import.is_empty() {
        return false;
    }
    if module == import {
        return true;
    }
    if import.len() > 1
        && module.len() > import.len()
        && module[module.len() - import.len()..] == import[..]
    {
        return true;
    }
    import.len() == 1 && module.first() == import.first()
}

fn without_mimi_suffix(path: &str) -> &str {
    path.strip_suffix(".mimi").unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::{module_path_from_source_key, module_path_matches_import_path};
    use crate::span::SourceKey;

    #[test]
    fn imported_module_identity_requires_exact_or_path_suffix_match() {
        let stdlib = SourceKey::new("stdlib:maps.mimi").expect("valid stdlib key");
        let nested =
            SourceKey::new("workspace:apps/demo/helpers/text.mimi").expect("valid workspace key");
        assert_eq!(
            module_path_from_source_key(&stdlib).as_deref(),
            Some("std::maps")
        );
        assert_eq!(
            module_path_from_source_key(&nested).as_deref(),
            Some("apps::demo::helpers::text")
        );
        assert!(module_path_matches_import_path(
            "apps::demo::helpers::text",
            "helpers::text"
        ));
        assert!(!module_path_matches_import_path(
            "apps::demo::helpers::text",
            "other::text"
        ));
        assert!(!module_path_matches_import_path(
            "apps::demo::helpers::text",
            "text"
        ));
    }
}

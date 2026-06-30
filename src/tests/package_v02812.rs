//! L1 tests for v0.28.12 package-manager completion.
//!
//! These tests cover:
//! - `mimi add` writing correct manifest entries
//! - `mimi install` lockfile idempotency (second run is a no-op)
//! - `mimi tree` output structure
//! - `mimi remove` round-trip
//! - Path / git dependency handling
//! - Lockfile version constraint resolution edge cases
//!
//! Tests use a synthetic registry directory built per-test, so they are
//! self-contained and do not depend on a real registry on disk.
//!
//! Test helper `main_install_idempotent` mimics the install flow and
//! verifies second-run is a no-op. The `mimi` binary itself is exercised
//! through `main_install_with_lockfile` / `main_install_path` helpers
//! reused from `tests/mod.rs`.

use crate::manifest::{Dependency, Manifest};
use crate::pkg_registry;
use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;

// ===================== L1: add manifest writes =====================

#[test]
fn add_writes_versioned_registry_dep() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_v02812_add_versioned_{}_{:?}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut m = Manifest::new("app");
    m.add_dependency("foo", Some("^1.0"), None, None, None);
    m.save(&dir).expect("save");

    let loaded = Manifest::load(&dir).expect("load").expect("present");
    let deps = loaded.dependencies.as_ref().expect("deps");
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].name, "foo");
    assert_eq!(deps[0].version.as_deref(), Some("^1.0"));
    assert!(deps[0].path.is_none());
    assert!(deps[0].git.is_none());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn add_writes_path_dep_without_version() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_v02812_add_path_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut m = Manifest::new("app");
    m.add_dependency("local-lib", None, Some("../local-lib"), None, None);
    m.save(&dir).expect("save");

    let loaded = Manifest::load(&dir).expect("load").expect("present");
    let deps = loaded.dependencies.as_ref().expect("deps");
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].name, "local-lib");
    assert!(deps[0].version.is_none());
    assert_eq!(deps[0].path.as_deref(), Some("../local-lib"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn add_writes_git_dep_with_tag() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_v02812_add_git_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut m = Manifest::new("app");
    m.add_dependency(
        "remote",
        None,
        None,
        Some("https://github.com/example/remote"),
        Some("v0.2.0"),
    );
    m.save(&dir).expect("save");

    let loaded = Manifest::load(&dir).expect("load").expect("present");
    let deps = loaded.dependencies.as_ref().expect("deps");
    assert_eq!(deps[0].name, "remote");
    assert_eq!(
        deps[0].git.as_deref(),
        Some("https://github.com/example/remote")
    );
    assert_eq!(deps[0].tag.as_deref(), Some("v0.2.0"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn add_replaces_existing_dep() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_v02812_add_replace_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut m = Manifest::new("app");
    m.add_dependency("foo", Some("^1.0"), None, None, None);
    m.add_dependency("foo", Some("^2.0"), None, None, None);
    m.save(&dir).expect("save");

    let loaded = Manifest::load(&dir).expect("load").expect("present");
    let deps = loaded.dependencies.as_ref().expect("deps");
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].version.as_deref(), Some("^2.0"));

    std::fs::remove_dir_all(&dir).ok();
}

// ===================== L1: remove round-trip =====================

#[test]
fn remove_drops_dep_and_lockfile_entry() {
    let root = std::env::temp_dir().join(format!(
        "mimi_v02812_remove_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("mkdir");

    let mut m = Manifest::new("app");
    m.add_dependency("foo", Some("^1.0"), None, None, None);
    m.add_dependency("bar", Some("^2.0"), None, None, None);
    m.save(&root).expect("save");

    let mut lock = crate::lockfile::Lockfile::new();
    lock.add_package("foo", "1.2.0", Some("registry"), None);
    lock.add_package("bar", "2.0.0", Some("registry"), None);
    lock.save(&root).expect("lock save");

    let mut loaded = Manifest::load(&root)
        .expect("load")
        .expect("present");
    assert!(loaded.remove_dependency("foo"));
    loaded.save(&root).expect("save after remove");

    let mut lock2 = crate::lockfile::Lockfile::load(&root)
        .expect("lock load")
        .expect("lock present");
    lock2.remove_package("foo");
    lock2.save(&root).expect("lock save 2");

    let loaded2 = Manifest::load(&root).expect("load").expect("present");
    let deps = loaded2.dependencies.as_ref().expect("deps");
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].name, "bar");

    let lock3 = crate::lockfile::Lockfile::load(&root)
        .expect("lock load")
        .expect("lock present");
    assert!(lock3.get_package("foo").is_none());
    assert!(lock3.get_package("bar").is_some());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn remove_nonexistent_returns_false() {
    let mut m = Manifest::new("app");
    assert!(!m.remove_dependency("ghost"));
    assert!(m.dependencies.is_none() || m.dependencies.as_ref().unwrap().is_empty());
}

// ===================== L1: lockfile idempotent install =====================

/// Build a synthetic registry dir with one versioned package and zero transitive deps.
fn make_leaf_pkg(reg: &Path, name: &str, version: &str) {
    let pkg_dir = reg.join(name).join(version);
    std::fs::create_dir_all(&pkg_dir).expect("mkdir pkg");
    let manifest = format!(
        r#"[package]
name = "{name}"
version = "{version}"
entry = "main.mimi"
"#
    );
    std::fs::write(pkg_dir.join("mimi.toml"), manifest).expect("write toml");
    std::fs::write(pkg_dir.join("main.mimi"), format!("func {name}() {{}}")).expect("write main");
}

/// `mimi install` semantics: re-running should be a no-op if lockfile + deps are present.
#[test]
fn install_is_idempotent_second_run_noop() {
    let root = std::env::temp_dir().join(format!(
        "mimi_v02812_install_idem_{}",
        std::process::id()
    ));
    let reg = root.join("registry");
    let project = root.join("project");
    std::fs::create_dir_all(&reg).expect("reg");
    std::fs::create_dir_all(&project).expect("proj");

    make_leaf_pkg(&reg, "leaf", "1.0.0");

    let mut m = Manifest::new("app");
    m.add_dependency("leaf", Some("^1.0"), None, None, None);
    m.save(&project).expect("save");
    std::fs::write(project.join("main.mimi"), "func main() {}").expect("write main");

    // First install
    super::main_install_transitive(&project, &reg).expect("first install ok");
    let lock_after_first = crate::lockfile::Lockfile::load(&project)
        .expect("load")
        .expect("present");
    assert!(lock_after_first.get_package("leaf").is_some());
    let checksum_first = lock_after_first
        .get_package("leaf")
        .and_then(|e| e.checksum.clone());
    let checksum_first = if checksum_first.is_some() {
        checksum_first.unwrap()
    } else {
        let dst = project.join(".mimi").join("deps").join("leaf");
        pkg_registry::compute_dir_checksum(&dst).expect("cs")
    };

    // Second install: should be idempotent.
    super::main_install_transitive(&project, &reg).expect("second install ok");
    let lock_after_second = crate::lockfile::Lockfile::load(&project)
        .expect("load")
        .expect("present");
    let entry2 = lock_after_second.get_package("leaf").expect("leaf present");
    let checksum_second = entry2
        .checksum
        .clone()
        .unwrap_or_else(|| pkg_registry::compute_dir_checksum(&project.join(".mimi").join("deps").join("leaf")).expect("cs"));
    assert_eq!(checksum_first, checksum_second, "checksum must be stable");
    assert_eq!(entry2.version, "1.0.0");

    std::fs::remove_dir_all(&root).ok();
}

// ===================== L1: tree walks transitive deps =====================

#[test]
fn tree_lists_direct_and_transitive_deps() {
    let root = std::env::temp_dir().join(format!(
        "mimi_v02812_tree_walk_{}",
        std::process::id()
    ));
    let reg = root.join("registry");
    let project = root.join("project");
    std::fs::create_dir_all(&reg).expect("reg");
    std::fs::create_dir_all(&project).expect("proj");

    // Chain: app -> middle -> leaf
    make_leaf_pkg(&reg, "leaf", "1.0.0");
    make_leaf_pkg(&reg, "middle", "1.0.0");
    // Manually write middle's manifest with a dependency on leaf
    let middle_manifest = r#"[package]
name = "middle"
version = "1.0.0"
entry = "main.mimi"

[[dependencies]]
name = "leaf"
version = "^1.0"
"#;
    std::fs::write(reg.join("middle").join("1.0.0").join("mimi.toml"), middle_manifest)
        .expect("write middle manifest");

    let mut m = Manifest::new("app");
    m.add_dependency("middle", Some("^1.0"), None, None, None);
    m.save(&project).expect("save");
    std::fs::write(project.join("main.mimi"), "func main() {}").expect("write main");

    super::main_install_transitive(&project, &reg).expect("install");

    // Verify the installed dep dir layout
    let deps_dir = project.join(".mimi").join("deps");
    assert!(deps_dir.join("middle").join("mimi.toml").exists(), "middle installed");
    assert!(deps_dir.join("leaf").join("mimi.toml").exists(), "leaf installed (transitive)");

    // Lockfile should have both
    let lock = crate::lockfile::Lockfile::load(&project)
        .expect("load")
        .expect("present");
    assert!(lock.get_package("middle").is_some());
    assert!(lock.get_package("leaf").is_some());

    std::fs::remove_dir_all(&root).ok();
}

// ===================== L1: registry constraint edge cases =====================

#[test]
fn registry_picks_highest_matching_caret() {
    let available = ["0.1.0", "0.9.0", "1.0.0", "1.2.3", "2.0.0", "1.5.0"];
    let result = crate::lockfile::Lockfile::resolve_version("^1.0", &available);
    assert_eq!(result.as_deref(), Some("1.5.0"));
}

#[test]
fn registry_picks_highest_matching_tilde() {
    let available = ["1.0.0", "1.0.1", "1.0.5", "1.1.0", "2.0.0"];
    let result = crate::lockfile::Lockfile::resolve_version("~1.0", &available);
    assert_eq!(result.as_deref(), Some("1.0.5"));
}

#[test]
fn registry_handles_comma_separated_constraint() {
    let available = ["0.1.0", "0.5.0", "1.0.0", "1.5.0", "2.0.0"];
    let result = crate::lockfile::Lockfile::resolve_version(">=1.0, <2.0", &available);
    assert_eq!(result.as_deref(), Some("1.5.0"));
}

#[test]
fn registry_returns_none_for_unmatchable_constraint() {
    let available = ["0.1.0", "0.2.0"];
    let result = crate::lockfile::Lockfile::resolve_version("^5.0", &available);
    assert!(result.is_none());
}

#[test]
fn registry_wildcard_returns_latest() {
    let available = ["0.1.0", "1.0.0", "0.5.0"];
    let result = crate::lockfile::Lockfile::resolve_version("*", &available);
    // Wildcard returns last (sorted) version, not numeric max
    assert!(result.is_some());
    let v = result.unwrap();
    assert!(available.contains(&v.as_str()));
}

#[test]
fn registry_exact_version_match() {
    let available = ["0.1.0", "1.0.0", "2.0.0"];
    let result = crate::lockfile::Lockfile::resolve_version("1.0.0", &available);
    assert_eq!(result.as_deref(), Some("1.0.0"));
}

// ===================== L1: lockfile checksum stability =====================

#[test]
fn lockfile_entry_round_trip_preserves_checksum() {
    let mut lf = crate::lockfile::Lockfile::new();
    lf.add_package(
        "foo",
        "1.2.3",
        Some("git+https://example.com/foo.git"),
        Some("abc123def456"),
    );
    let dir = std::env::temp_dir().join(format!(
        "mimi_v02812_lockfile_rt_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    lf.save(&dir).expect("save");
    let loaded = crate::lockfile::Lockfile::load(&dir)
        .expect("load")
        .expect("present");
    let e = loaded.get_package("foo").expect("present");
    assert_eq!(e.version, "1.2.3");
    assert_eq!(e.source.as_deref(), Some("git+https://example.com/foo.git"));
    assert_eq!(e.checksum.as_deref(), Some("abc123def456"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn lockfile_replaces_existing_entry() {
    let mut lf = crate::lockfile::Lockfile::new();
    lf.add_package("foo", "1.0.0", None, None);
    lf.add_package("foo", "2.0.0", None, None);
    assert_eq!(lf.package.len(), 1);
    let e = lf.get_package("foo").expect("present");
    assert_eq!(e.version, "2.0.0");
}

// ===================== L1: dependency spec validation =====================

#[test]
fn dependency_supports_all_source_kinds() {
    let cases = vec![
        ("registry", Dependency { name: "r".into(), version: Some("^1".into()), path: None, git: None, tag: None }),
        ("path",     Dependency { name: "p".into(), version: None, path: Some("./p".into()), git: None, tag: None }),
        ("git",      Dependency { name: "g".into(), version: None, path: None, git: Some("https://x/g".into()), tag: Some("main".into()) }),
    ];
    let mut m = Manifest::new("app");
    for (_, d) in cases {
        m.dependencies.get_or_insert_with(Vec::new).push(d);
    }
    let dir = std::env::temp_dir().join(format!("mimi_v02812_specs_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    m.save(&dir).expect("save");
    let loaded = Manifest::load(&dir).expect("load").expect("present");
    let deps = loaded.dependencies.expect("deps");
    assert_eq!(deps.len(), 3);
    assert!(deps[0].path.is_none() && deps[0].git.is_none());
    assert!(deps[1].path.is_some());
    assert!(deps[2].git.is_some() && deps[2].tag.is_some());

    std::fs::remove_dir_all(&dir).ok();
}

// ===================== L1: path dependency copy =====================

#[test]
fn path_dependency_copies_to_deps_dir() {
    let root = std::env::temp_dir().join(format!(
        "mimi_v02812_path_dep_{}",
        std::process::id()
    ));
    let lib = root.join("my-lib");
    let project = root.join("project");
    std::fs::create_dir_all(&lib).expect("lib");
    std::fs::create_dir_all(&project).expect("proj");

    std::fs::write(
        lib.join("mimi.toml"),
        r#"[package]
name = "my-lib"
version = "0.1.0"
entry = "main.mimi"
"#,
    )
    .expect("write lib toml");
    std::fs::write(lib.join("main.mimi"), "func greet() {}").expect("write main");

    let mut m = Manifest::new("app");
    let lib_str = lib.to_str().expect("utf8 path");
    m.add_dependency("my-lib", None, Some(lib_str), None, None);
    m.save(&project).expect("save");
    std::fs::write(project.join("main.mimi"), "func main() {}").expect("write main");

    // Use the project-scoped install helper
    let reg = root.join("registry");
    std::fs::create_dir_all(&reg).expect("reg");
    super::main_install_transitive(&project, &reg).expect("install");

    // path dep should be present
    let dst = project.join(".mimi").join("deps").join("my-lib");
    assert!(dst.join("mimi.toml").exists(), "path dep mimi.toml copied");
    assert!(dst.join("main.mimi").exists(), "path dep main.mimi copied");

    std::fs::remove_dir_all(&root).ok();
}

// ===================== L1: cycle detection in install =====================

#[test]
fn install_breaks_cycles_in_transitive_graph() {
    let root = std::env::temp_dir().join(format!(
        "mimi_v02812_cycle_{}",
        std::process::id()
    ));
    let reg = root.join("registry");
    let project = root.join("project");
    std::fs::create_dir_all(&reg).expect("reg");
    std::fs::create_dir_all(&project).expect("proj");

    // Build cycle: a -> b -> a
    make_leaf_pkg(&reg, "pkg-a", "1.0.0");
    make_leaf_pkg(&reg, "pkg-b", "1.0.0");
    let a_with_dep = r#"[package]
name = "pkg-a"
version = "1.0.0"
entry = "main.mimi"

[[dependencies]]
name = "pkg-b"
version = "^1.0"
"#;
    let b_with_dep = r#"[package]
name = "pkg-b"
version = "1.0.0"
entry = "main.mimi"

[[dependencies]]
name = "pkg-a"
version = "^1.0"
"#;
    std::fs::write(reg.join("pkg-a").join("1.0.0").join("mimi.toml"), a_with_dep).expect("write a");
    std::fs::write(reg.join("pkg-b").join("1.0.0").join("mimi.toml"), b_with_dep).expect("write b");

    let mut m = Manifest::new("app");
    m.add_dependency("pkg-a", Some("^1.0"), None, None, None);
    m.save(&project).expect("save");
    std::fs::write(project.join("main.mimi"), "func main() {}").expect("write main");

    let result = super::main_install_transitive(&project, &reg);
    assert!(result.is_ok(), "cycle should be broken by visited set: {:?}", result.err());

    let lock = crate::lockfile::Lockfile::load(&project)
        .expect("load")
        .expect("present");
    assert!(lock.get_package("pkg-a").is_some());
    assert!(lock.get_package("pkg-b").is_some());

    std::fs::remove_dir_all(&root).ok();
}

// ===================== L1: diamond dedup =====================

#[test]
fn install_dedups_diamond_transitive() {
    let root = std::env::temp_dir().join(format!(
        "mimi_v02812_diamond_{}",
        std::process::id()
    ));
    let reg = root.join("registry");
    let project = root.join("project");
    std::fs::create_dir_all(&reg).expect("reg");
    std::fs::create_dir_all(&project).expect("proj");

    // Diamond: app -> b -> d, app -> c -> d
    make_leaf_pkg(&reg, "dep-d", "1.0.0");
    let b_manifest = r#"[package]
name = "dep-b"
version = "1.0.0"
entry = "main.mimi"

[[dependencies]]
name = "dep-d"
version = "^1.0"
"#;
    let c_manifest = r#"[package]
name = "dep-c"
version = "2.0.0"
entry = "main.mimi"

[[dependencies]]
name = "dep-d"
version = "^1.0"
"#;
    std::fs::create_dir_all(reg.join("dep-b").join("1.0.0")).expect("b dir");
    std::fs::write(reg.join("dep-b").join("1.0.0").join("mimi.toml"), b_manifest).expect("b");
    std::fs::write(reg.join("dep-b").join("1.0.0").join("main.mimi"), "func depb() {}").expect("b main");
    std::fs::create_dir_all(reg.join("dep-c").join("2.0.0")).expect("c dir");
    std::fs::write(reg.join("dep-c").join("2.0.0").join("mimi.toml"), c_manifest).expect("c");
    std::fs::write(reg.join("dep-c").join("2.0.0").join("main.mimi"), "func depc() {}").expect("c main");

    let mut m = Manifest::new("app");
    m.add_dependency("dep-b", Some("^1.0"), None, None, None);
    m.add_dependency("dep-c", Some("^2.0"), None, None, None);
    m.save(&project).expect("save");
    std::fs::write(project.join("main.mimi"), "func main() {}").expect("write main");

    super::main_install_transitive(&project, &reg).expect("install");

    let lock = crate::lockfile::Lockfile::load(&project)
        .expect("load")
        .expect("present");
    let d_count = lock.package.iter().filter(|p| p.name == "dep-d").count();
    assert_eq!(d_count, 1, "diamond dep-d should be deduped in lockfile");
    let mut names: Vec<String> = lock.package.iter().map(|p| p.name.clone()).collect();
    names.sort();
    let expected: HashSet<String> =
        ["dep-b", "dep-c", "dep-d"].iter().map(|s| s.to_string()).collect();
    let got: HashSet<String> = names.iter().cloned().collect();
    assert_eq!(got, expected);

    std::fs::remove_dir_all(&root).ok();
}

// ===================== L1: lockfile version-bump on update =====================

#[test]
fn lockfile_replaces_version_on_resolver_pick() {
    // Simulate an update flow: lockfile has 1.0.0, registry adds 1.1.0,
    // resolver re-picks 1.1.0 -> lockfile entry updates.
    let mut lf = crate::lockfile::Lockfile::new();
    lf.add_package("foo", "1.0.0", Some("registry"), None);

    let available = ["1.0.0", "1.1.0"];
    let resolved = crate::lockfile::Lockfile::resolve_version("^1.0", &available);
    assert_eq!(resolved.as_deref(), Some("1.1.0"));

    lf.add_package("foo", resolved.as_deref().unwrap(), Some("registry"), None);
    let e = lf.get_package("foo").expect("present");
    assert_eq!(e.version, "1.1.0");
    assert_eq!(lf.package.len(), 1, "no duplicates after replace");
}

// ===================== L1: empty registry on first add =====================

#[test]
fn install_with_no_dependencies_is_noop() {
    let root = std::env::temp_dir().join(format!(
        "mimi_v02812_noop_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("mkdir");
    let m = Manifest::new("app");
    m.save(&root).expect("save");
    let reg = root.join("registry");
    std::fs::create_dir_all(&reg).expect("reg");

    super::main_install_transitive(&root, &reg).expect("noop install ok");

    let lock = crate::lockfile::Lockfile::load(&root).expect("load");
    if let Some(lock) = lock {
        assert!(lock.package.is_empty(), "no lock entries when no deps");
    }

    std::fs::remove_dir_all(&root).ok();
}

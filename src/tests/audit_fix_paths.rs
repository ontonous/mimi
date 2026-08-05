//! Wave-1 audit-fix regression tests — paths.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
//!
//! Wave-2 additions (prefix `audit2_pkg_`): devdocs/full-audit-2026-08-05-0656.md
//! §2.9 H-30 (dependency-name path traversal in `mimi install`) and §3.10
//! X-1 (transitive path-dep base), X-2 (session cycle DoS), X-3 (lockfile
//! `*` wildcard picks non-semver dirs).

use crate::path_safety::{self, PathError};
use crate::pkg_registry;

// ===================== §13: git dep URL scheme allow-list =====================

#[test]
fn audit_paths_git_url_https_and_git_at_accepted() {
    // full-audit 2026-08-05 §13 [LOW-MED]: https:// stays allowed, and the
    // pre-existing git@ scp-style ssh form remains supported.
    assert!(path_safety::validate_git_url("https://github.com/user/repo.git").is_ok());
    assert!(path_safety::validate_git_url("https://gitlab.com/group/sub/repo").is_ok());
    assert!(path_safety::validate_git_url("git@github.com:user/repo.git").is_ok());
}

#[test]
fn audit_paths_git_url_http_and_file_rejected() {
    // full-audit 2026-08-05 §13 [LOW-MED]: http:// = plaintext dependency MITM,
    // file:// = local-repo exfiltration surface via crafted manifests.
    assert_eq!(
        path_safety::validate_git_url("http://github.com/user/repo.git"),
        Err(PathError::ForbiddenProtocol)
    );
    assert_eq!(
        path_safety::validate_git_url("file:///home/user/repos/private"),
        Err(PathError::ForbiddenProtocol)
    );
    // Plaintext/unauthenticated git:// is the same MITM class as http://.
    assert_eq!(
        path_safety::validate_git_url("git://mirrors.example.com/repo"),
        Err(PathError::ForbiddenProtocol)
    );
    // ssh:// is outside the allow-list (use the git@ scp form or https).
    assert_eq!(
        path_safety::validate_git_url("ssh://git@github.com/user/repo.git"),
        Err(PathError::ForbiddenProtocol)
    );
    // Regression guards: pre-existing injections still rejected.
    assert_eq!(
        path_safety::validate_git_url("ext::sh -c 'true'"),
        Err(PathError::ForbiddenProtocol)
    );
    assert_eq!(
        path_safety::validate_git_url("-uupload-pack"),
        Err(PathError::OptionInjection)
    );
    // The error must state the allow-list so users can self-correct.
    let msg = path_safety::validate_git_url("http://x/y")
        .unwrap_err()
        .to_string();
    assert!(
        msg.contains("https://") && msg.contains("git@"),
        "error should name the allowed forms: {msg}"
    );
}

// ===================== §13: dot/blank package names join over deps root =====================

#[test]
fn audit_paths_package_name_dot_forms_rejected() {
    // full-audit 2026-08-05 §13 [LOW-MED]: deps_dir.join(".") installs over
    // the deps root itself. Reject ".", "..", "" and dot/whitespace-only names.
    assert_eq!(
        path_safety::validate_package_name("."),
        Err(PathError::InvalidName)
    );
    assert_eq!(
        path_safety::validate_package_name(".."),
        Err(PathError::InvalidName)
    );
    assert_eq!(
        path_safety::validate_package_name(""),
        Err(PathError::Empty)
    );
    assert_eq!(
        path_safety::validate_package_name("..."),
        Err(PathError::InvalidName)
    );
    assert_eq!(
        path_safety::validate_package_name(" . "),
        Err(PathError::InvalidName)
    );
    assert_eq!(
        path_safety::validate_package_name("   "),
        Err(PathError::InvalidName)
    );
}

#[test]
fn audit_paths_package_name_normal_still_accepted() {
    // No over-rejection: ordinary names (including internal dots) still pass.
    assert!(path_safety::validate_package_name("my-pkg").is_ok());
    assert!(path_safety::validate_package_name("my_pkg_123").is_ok());
    assert!(path_safety::validate_package_name("foo.bar").is_ok());
    assert!(path_safety::validate_package_name("v1.2.3").is_ok());
    // Pre-existing rejections unchanged.
    assert_eq!(
        path_safety::validate_package_name("../evil"),
        Err(PathError::InvalidName)
    );
    assert_eq!(
        path_safety::validate_package_name("a/b"),
        Err(PathError::InvalidName)
    );
    assert_eq!(
        path_safety::validate_package_name("a\\b"),
        Err(PathError::InvalidName)
    );
}

// ===================== §13: checksum must fail closed on unreadable files =====================

#[test]
#[cfg(unix)]
fn audit_paths_checksum_unreadable_file_fails_closed() {
    // full-audit 2026-08-05 §13 [LOW-MED]: a file that cannot be read must
    // abort the checksum with Err (integrity cannot be guaranteed), never
    // warning+skip — skipped files made the recorded checksum depend on local
    // file permissions and diverge across machines.
    use std::os::unix::fs::PermissionsExt;

    // Root ignores permission bits; the scenario is unreachable there.
    // SAFETY: geteuid is a thread-safe, total POSIX call with no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let dir = std::env::temp_dir().join(format!("mimi_audit_paths_cs_perm_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("ok.txt"), "readable").expect("w ok");
    let secret = dir.join("secret.txt");
    std::fs::write(&secret, "cannot read").expect("w secret");

    // Baseline: fully readable dir checksums fine.
    let baseline = pkg_registry::compute_dir_checksum(&dir);
    assert!(baseline.is_ok(), "readable dir must checksum: {baseline:?}");

    // Make one file unreadable → the checksum must fail closed with the path.
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");
    let err = pkg_registry::compute_dir_checksum(&dir)
        .expect_err("unreadable file must abort the checksum, not be skipped");
    assert!(
        err.contains("secret.txt"),
        "error must name the unreadable file: {err}"
    );

    // Restoring readability restores success — proves the failure was the
    // permission, and the fix is exactly fail-closed (no silent divergence).
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).expect("chmod 644");
    let restored = pkg_registry::compute_dir_checksum(&dir);
    assert!(
        restored.is_ok(),
        "restored perms must checksum: {restored:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ===========================================================================
// Wave-2 (2026-08-05 second audit): package-manager security — H-30, X-1..X-3
// ===========================================================================

/// Locate the debug mimi binary (same convention as package_management.rs).
fn audit2_pkg_mimi_bin() -> Option<std::path::PathBuf> {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target/debug/mimi");
    if p.exists() {
        return Some(p);
    }
    p.set_extension("exe");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

/// Recursively collect every file named `name` under `dir`.
fn audit2_pkg_find_files(dir: &std::path::Path, name: &str) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(audit2_pkg_find_files(&path, name));
        } else if entry.file_name() == name {
            out.push(path);
        }
    }
    out
}

// ---------- H-30: dependency names must validate in ALL resolution branches ----------

#[test]
fn audit2_pkg_resolver_rejects_traversal_name_in_every_branch() {
    // 0656 §2.9 H-30: only the registry branch validated `dep.name`; the
    // git/path branches had zero validation before `deps_dir.join(&dep.name)`.
    // The choke point in `resolve_single_dep_in` must now reject traversal
    // names for registry, git and path sources alike — before any FS write.
    let root = std::env::temp_dir().join(format!(
        "mimi_audit2_pkg_h30_resolver_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let deps_dir = root.join("project/.mimi/deps");
    std::fs::create_dir_all(&deps_dir).expect("mkdir deps");
    // Payload that a traversal write would install.
    std::fs::create_dir_all(root.join("payload-src")).expect("mkdir payload");
    std::fs::write(root.join("payload-src/ATTACK_PROBE.txt"), "pwned").expect("w probe");
    let reg = root.join("registry");
    std::fs::create_dir_all(&reg).expect("mkdir reg");

    for (branch, dep) in [
        (
            "path",
            crate::manifest::Dependency {
                name: "../pwned".into(),
                version: None,
                path: Some("payload-src".into()),
                git: None,
                tag: None,
            },
        ),
        (
            "git",
            crate::manifest::Dependency {
                name: "../../pwned".into(),
                version: None,
                path: None,
                git: Some("https://example.com/evil.git".into()),
                tag: None,
            },
        ),
        (
            "registry",
            crate::manifest::Dependency {
                name: "..\\pwned".into(),
                version: Some("*".into()),
                path: None,
                git: None,
                tag: None,
            },
        ),
    ] {
        let dst = deps_dir.join(&dep.name);
        let err = crate::pkg_resolve::resolve_single_dep_in(&dep, &dst, &reg, Some(&root))
            .expect_err(&format!("{branch}: traversal name must be rejected"));
        assert!(
            err.contains("invalid package name"),
            "{branch}: error must name the invalid package name, got: {err}"
        );
    }

    // No traversal write may have happened anywhere outside the deps dir.
    let outside: Vec<_> = audit2_pkg_find_files(&root, "ATTACK_PROBE.txt")
        .into_iter()
        .filter(|p| !p.starts_with(root.join("payload-src")))
        .collect();
    assert!(
        outside.is_empty(),
        "resolver wrote the payload outside the source fixture: {outside:?}"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn audit2_pkg_install_cli_direct_traversal_write_blocked() {
    // 0656 §2.9 H-30 end-to-end PoC: `name = "../../pwned"` + path payload.
    // Pre-fix, `mimi install` copied the payload to <project>/pwned/ (exit 0).
    // Post-fix: install refuses with a clear error and writes nothing.
    let Some(bin) = audit2_pkg_mimi_bin() else {
        eprintln!("skip: mimi binary not built");
        return;
    };
    let root = std::env::temp_dir().join(format!(
        "mimi_audit2_pkg_h30_cli_direct_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let proj = root.join("proj");
    std::fs::create_dir_all(&home).expect("mkdir home");
    std::fs::create_dir_all(proj.join("payload-src")).expect("mkdir payload");
    std::fs::write(proj.join("payload-src/ATTACK_PROBE.txt"), "pwned").expect("w probe");
    std::fs::write(
        proj.join("payload-src/mimi.toml"),
        "[package]\nname = \"payload\"\nversion = \"0.1.0\"\n",
    )
    .expect("w payload manifest");
    std::fs::write(
        proj.join("mimi.toml"),
        "[package]\nname = \"proj\"\nversion = \"0.1.0\"\n\n\
         [[dependencies]]\nname = \"../../pwned\"\npath = \"payload-src\"\n",
    )
    .expect("w manifest");
    std::fs::write(proj.join("main.mimi"), "func main() {}\n").expect("w main");

    let out = std::process::Command::new(&bin)
        .arg("install")
        .current_dir(&proj)
        .env("HOME", &home)
        .output()
        .expect("spawn mimi install");
    assert!(
        !out.status.success(),
        "traversal install must fail; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid dependency name") || stderr.contains("invalid package name"),
        "stderr must report the bad name: {stderr}"
    );
    // The probe file may exist ONLY inside the untouched source fixture.
    let leaked: Vec<_> = audit2_pkg_find_files(&root, "ATTACK_PROBE.txt")
        .into_iter()
        .filter(|p| !p.starts_with(proj.join("payload-src")))
        .collect();
    assert!(
        leaked.is_empty(),
        "install wrote the payload outside the deps source: {leaked:?}"
    );
    assert!(
        !proj.join("pwned").exists(),
        "project-root traversal target must not exist"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn audit2_pkg_install_cli_transitive_traversal_write_blocked() {
    // 0656 §2.9 H-30 supply-chain shape: a LEGITIMATE dependency's manifest
    // declares a sub-dependency whose name traverses. Pre-fix the payload was
    // written OUTSIDE the project root; post-fix the install must refuse.
    let Some(bin) = audit2_pkg_mimi_bin() else {
        eprintln!("skip: mimi binary not built");
        return;
    };
    let root = std::env::temp_dir().join(format!(
        "mimi_audit2_pkg_h30_cli_transitive_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let proj = root.join("proj");
    let goodpkg = root.join("goodpkg");
    let payload = root.join("payload");
    for d in [&home, &proj, &goodpkg, &payload] {
        std::fs::create_dir_all(d).expect("mkdir");
    }
    std::fs::write(payload.join("ATTACK_PROBE.txt"), "pwned").expect("w probe");
    std::fs::write(
        payload.join("mimi.toml"),
        "[package]\nname = \"payload\"\nversion = \"0.1.0\"\n",
    )
    .expect("w payload manifest");
    std::fs::write(
        goodpkg.join("mimi.toml"),
        "[package]\nname = \"goodpkg\"\nversion = \"0.1.0\"\n\n\
         [[dependencies]]\nname = \"../../../pwned2\"\npath = \"../payload\"\n",
    )
    .expect("w goodpkg manifest");
    std::fs::write(goodpkg.join("main.mimi"), "func main() {}\n").expect("w goodpkg main");
    std::fs::write(
        proj.join("mimi.toml"),
        "[package]\nname = \"proj2\"\nversion = \"0.1.0\"\n\n\
         [[dependencies]]\nname = \"goodpkg\"\npath = \"../goodpkg\"\n",
    )
    .expect("w manifest");
    std::fs::write(proj.join("main.mimi"), "func main() {}\n").expect("w main");

    let out = std::process::Command::new(&bin)
        .arg("install")
        .current_dir(&proj)
        .env("HOME", &home)
        .output()
        .expect("spawn mimi install");
    assert!(
        !out.status.success(),
        "transitive traversal install must fail; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid dependency name") || stderr.contains("invalid package name"),
        "stderr must report the bad transitive name: {stderr}"
    );
    let leaked: Vec<_> = audit2_pkg_find_files(&root, "ATTACK_PROBE.txt")
        .into_iter()
        .filter(|p| !p.starts_with(&payload))
        .collect();
    assert!(
        leaked.is_empty(),
        "transitive install wrote the payload outside its source: {leaked:?}"
    );
    assert!(
        !root.join("pwned2").exists() && !proj.join("pwned2").exists(),
        "traversal targets must not exist"
    );
    std::fs::remove_dir_all(&root).ok();
}

// ---------- X-1: transitive path deps resolve against the owning package ----------

#[test]
fn audit2_pkg_transitive_path_dep_base_is_owning_package_cli() {
    // 0656 §3.10 X-1: transitive `path` deps used to resolve against the
    // TOP-LEVEL project dir, so vendored sub-libraries of an installed
    // package were "not found". They must resolve against the owning
    // package's install directory instead.
    let Some(bin) = audit2_pkg_mimi_bin() else {
        eprintln!("skip: mimi binary not built");
        return;
    };
    let root = std::env::temp_dir().join(format!("mimi_audit2_pkg_x1_cli_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let proj = root.join("proj");
    let goodpkg = root.join("goodpkg");
    let vendored = goodpkg.join("vendored");
    for d in [&home, &proj, &goodpkg, &vendored] {
        std::fs::create_dir_all(d).expect("mkdir");
    }
    std::fs::write(
        vendored.join("mimi.toml"),
        "[package]\nname = \"vendored\"\nversion = \"0.1.0\"\n",
    )
    .expect("w vendored manifest");
    std::fs::write(vendored.join("lib.mimi"), "func main() {}\n").expect("w vendored lib");
    std::fs::write(
        goodpkg.join("mimi.toml"),
        "[package]\nname = \"goodpkg\"\nversion = \"0.1.0\"\n\n\
         [[dependencies]]\nname = \"vendored\"\npath = \"./vendored\"\n",
    )
    .expect("w goodpkg manifest");
    std::fs::write(goodpkg.join("main.mimi"), "func main() {}\n").expect("w goodpkg main");
    std::fs::write(
        proj.join("mimi.toml"),
        "[package]\nname = \"proj3\"\nversion = \"0.1.0\"\n\n\
         [[dependencies]]\nname = \"goodpkg\"\npath = \"../goodpkg\"\n",
    )
    .expect("w manifest");
    std::fs::write(proj.join("main.mimi"), "func main() {}\n").expect("w main");

    let out = std::process::Command::new(&bin)
        .arg("install")
        .current_dir(&proj)
        .env("HOME", &home)
        .output()
        .expect("spawn mimi install");
    assert!(
        out.status.success(),
        "install with vendored transitive path dep must succeed; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        proj.join(".mimi/deps/vendored/mimi.toml").exists(),
        "vendored dep must be installed relative to the owning package"
    );
    assert!(proj.join(".mimi/deps/goodpkg/mimi.toml").exists());
    std::fs::remove_dir_all(&root).ok();
}

// ---------- X-2: session definition cycles must not stack-overflow ----------

#[test]
fn audit2_pkg_session_cycle_resolve_fails_closed() {
    use crate::ast::SessionType;
    use std::collections::HashMap;

    fn name(n: &str) -> SessionType {
        SessionType::Name(n.to_string())
    }
    fn send_i32(cont: SessionType) -> SessionType {
        SessionType::Send(crate::ast::Type::Name("i32".into(), vec![]), Box::new(cont))
    }

    // Two-node cycle: A = B, B = A. Pre-fix this recursed forever.
    let mut env = HashMap::new();
    env.insert("A".to_string(), name("B"));
    env.insert("B".to_string(), name("A"));
    assert_eq!(crate::session::resolve(&name("A"), &env), None);
    assert_eq!(crate::session::resolve(&name("B"), &env), None);

    // Direct self-reference: A = A (previously special-cased one level deep).
    let mut env_self = HashMap::new();
    env_self.insert("A".to_string(), name("A"));
    assert_eq!(crate::session::resolve(&name("A"), &env_self), None);

    // Cycle through a continuation: A = !i32 . B, B = ?i32 . A.
    let mut env_cont = HashMap::new();
    env_cont.insert("A".to_string(), send_i32(name("B")));
    env_cont.insert(
        "B".to_string(),
        SessionType::Recv(
            crate::ast::Type::Name("i32".into(), vec![]),
            Box::new(name("A")),
        ),
    );
    assert_eq!(crate::session::resolve(&name("A"), &env_cont), None);

    // Acyclic chains and diamonds must still resolve (no over-rejection).
    let mut env_ok = HashMap::new();
    env_ok.insert("A".to_string(), name("B"));
    env_ok.insert("B".to_string(), name("C"));
    env_ok.insert("C".to_string(), send_i32(SessionType::End));
    let r = crate::session::resolve(&name("A"), &env_ok).expect("chain resolves");
    assert!(matches!(r, SessionType::Send(_, _)), "{r:?}");
    // Same target reached from a second root (path-stack pop on exit).
    let r2 = crate::session::resolve(&name("B"), &env_ok).expect("diamond resolves");
    assert!(matches!(r2, SessionType::Send(_, _)), "{r2:?}");
}

#[test]
fn audit2_pkg_session_cycle_detect_helper() {
    use crate::ast::SessionType;
    use std::collections::HashMap;

    // A <-> B
    let mut env = HashMap::new();
    env.insert("A".to_string(), SessionType::Name("B".into()));
    env.insert("B".to_string(), SessionType::Name("A".into()));
    let cycle = crate::session::detect_session_cycle(&env).expect("cycle must be found");
    assert_eq!(cycle.first(), cycle.last(), "cycle must close: {cycle:?}");
    assert!(cycle.len() >= 2, "cycle path includes at least one edge");
    assert!(cycle.contains(&"A".to_string()) && cycle.contains(&"B".to_string()));

    // Acyclic diamond shares a node but has no cycle.
    let mut env_diamond = HashMap::new();
    env_diamond.insert("A".to_string(), SessionType::Name("C".into()));
    env_diamond.insert("B".to_string(), SessionType::Name("C".into()));
    env_diamond.insert("C".to_string(), SessionType::End);
    assert_eq!(crate::session::detect_session_cycle(&env_diamond), None);

    // Cycle hiding inside a continuation: A = !i32 . B, B = ?i32 . A.
    let mut env_cont = HashMap::new();
    env_cont.insert(
        "A".to_string(),
        SessionType::Send(
            crate::ast::Type::Name("i32".into(), vec![]),
            Box::new(SessionType::Name("B".into())),
        ),
    );
    env_cont.insert(
        "B".to_string(),
        SessionType::Recv(
            crate::ast::Type::Name("i32".into(), vec![]),
            Box::new(SessionType::Name("A".into())),
        ),
    );
    assert!(crate::session::detect_session_cycle(&env_cont).is_some());

    // Long acyclic chain (1000 nodes): must terminate without overflow
    // (iterative DFS — the helper itself must be DoS-proof).
    let mut env_chain = HashMap::new();
    for i in 0..1000 {
        env_chain.insert(format!("N{i}"), SessionType::Name(format!("N{}", i + 1)));
    }
    env_chain.insert("N1000".to_string(), SessionType::End);
    assert_eq!(crate::session::detect_session_cycle(&env_chain), None);
}

#[test]
fn audit2_pkg_session_cycle_cli_no_stack_overflow() {
    // 0656 §3.10 X-2 PoC: `session A = B; session B = A` used anywhere
    // aborted the compiler with a stack overflow (exit 134). Post-fix it
    // must exit cleanly with a user-facing diagnostic — never a signal.
    let Some(bin) = audit2_pkg_mimi_bin() else {
        eprintln!("skip: mimi binary not built");
        return;
    };
    let root = std::env::temp_dir().join(format!("mimi_audit2_pkg_x2_cli_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");
    let src = root.join("cycle.mimi");
    std::fs::write(
        &src,
        "session A = B\nsession B = A\nfunc f(ch: SessionChan<A>) {\n    session_close(ch)\n}\nfunc main() {}\n",
    )
    .expect("w cycle.mimi");

    let out = std::process::Command::new(&bin)
        .args(["check"])
        .arg(&src)
        .env("HOME", &root)
        .output()
        .expect("spawn mimi check");
    // A signal kill (e.g. SIGABRT from stack overflow) yields no exit code.
    let code = out
        .status
        .code()
        .expect("compiler must exit normally, not die on a signal (stack overflow?)");
    assert_ne!(
        code, 0,
        "cyclic session used in a signature must not typecheck"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error"),
        "stderr must carry a diagnostic: {stderr}"
    );

    // Cycle-only source (no use) must compile without aborting either.
    let src_unused = root.join("cycle_unused.mimi");
    std::fs::write(
        &src_unused,
        "session A = B\nsession B = A\nfunc main() {}\n",
    )
    .expect("w");
    let out2 = std::process::Command::new(&bin)
        .args(["check"])
        .arg(&src_unused)
        .env("HOME", &root)
        .output()
        .expect("spawn mimi check (unused cycle)");
    out2.status.code().expect("must exit normally, not abort");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn audit2_pkg_session_acyclic_still_checkable() {
    // No over-rejection: alias chains (A = B, B = concrete) still resolve
    // through the checker exactly as before the cycle guard landed.
    let src = r#"
session B = !i32 . end
session A = B
func f(ch: SessionChan<A>) {
    session_send(ch, 1)
    session_close(ch)
}
func main() -> i32 { 0 }
"#;
    super::check_source(src).expect("acyclic session alias must typecheck");
}

// ---------- X-3: lockfile `*` must skip non-semver directories ----------

#[test]
fn audit2_pkg_wildcard_skips_non_semver_dirs() {
    use crate::lockfile::Lockfile;

    // Pre-fix the comparator sorted non-semver dirs LAST and `*` took the
    // tail, so `.git`/`latest` were installed as the "newest version".
    assert_eq!(
        Lockfile::resolve_version("*", &["1.0.0", "2.0.0", "latest", ".git"]).as_deref(),
        Some("2.0.0")
    );
    assert_eq!(
        Lockfile::resolve_version("*", &["0.9.0", "latest"]).as_deref(),
        Some("0.9.0")
    );
    // No valid semver at all → fail closed (mirrors the VersionReq branch).
    assert_eq!(Lockfile::resolve_version("*", &["latest", ".git"]), None);
    assert_eq!(Lockfile::resolve_version("*", &[]), None);
    // Empty constraint is treated as `*` and must skip non-semver too.
    assert_eq!(
        Lockfile::resolve_version("", &["3.1.0", "garbage"]).as_deref(),
        Some("3.1.0")
    );
    // Pre-releases are valid semver and outrank lower cores.
    assert_eq!(
        Lockfile::resolve_version("*", &["1.0.0", "2.0.0-alpha", "notes"]).as_deref(),
        Some("2.0.0-alpha")
    );
}

#[test]
fn audit2_pkg_wildcard_cli_ignores_latest_dir() {
    // 0656 §3.10 X-3 end-to-end: a registry with a stray `latest` directory
    // installed that directory under `version = "*"`. Post-fix the highest
    // real semver (2.0.0) must win.
    let Some(bin) = audit2_pkg_mimi_bin() else {
        eprintln!("skip: mimi binary not built");
        return;
    };
    let root = std::env::temp_dir().join(format!("mimi_audit2_pkg_x3_cli_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let reg = home.join(".mimi/registry/x3pkg");
    for v in ["1.0.0", "2.0.0", "latest"] {
        std::fs::create_dir_all(reg.join(v)).expect("mkdir version dir");
        std::fs::write(
            reg.join(v).join("mimi.toml"),
            format!("[package]\nname = \"x3pkg\"\nversion = \"{v}\"\n"),
        )
        .expect("w version manifest");
        std::fs::write(reg.join(v).join("probe.txt"), format!("marker {v}\n")).expect("w probe");
    }
    let proj = root.join("proj");
    std::fs::create_dir_all(&proj).expect("mkdir proj");
    std::fs::write(
        proj.join("mimi.toml"),
        "[package]\nname = \"proj4\"\nversion = \"0.1.0\"\n\n\
         [[dependencies]]\nname = \"x3pkg\"\nversion = \"*\"\n",
    )
    .expect("w manifest");
    std::fs::write(proj.join("main.mimi"), "func main() {}\n").expect("w main");

    let out = std::process::Command::new(&bin)
        .arg("install")
        .current_dir(&proj)
        .env("HOME", &home)
        .output()
        .expect("spawn mimi install");
    assert!(
        out.status.success(),
        "install must succeed; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lock = std::fs::read_to_string(proj.join("mimi.lock")).expect("lockfile written");
    assert!(
        lock.contains("version = \"2.0.0\""),
        "wildcard must resolve to the highest semver, lock was: {lock}"
    );
    assert!(
        !lock.contains("version = \"latest\""),
        "non-semver `latest` dir must not be picked: {lock}"
    );
    let probe = std::fs::read_to_string(proj.join(".mimi/deps/x3pkg/probe.txt"))
        .expect("installed probe readable");
    assert_eq!(probe.trim(), "marker 2.0.0");
    std::fs::remove_dir_all(&root).ok();
}

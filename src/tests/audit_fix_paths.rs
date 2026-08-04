//! Wave-1 audit-fix regression tests — paths.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).

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

/// Development-time invariant check.
///
/// - In `debug_assertions` builds (debug + test): if `$cond` is false, emits a
///   warning via `eprintln!` and then panics via `debug_assert!`.
/// - In release builds: completely elided at compile time (zero binary cost).
///
/// This is stricter than nih-plug's approach (which only warns in debug mode)
/// because a compiler ICE is *good* — crashing with a clear message is safer
/// than generating incorrect code.  The `eprintln!` warning ensures the
/// violation is visible in logs even when running in a context that swallows
/// panics (e.g. an LSP server).
///
/// # When to use
///
/// Use this for invariants that "should always hold; if they don't, the
/// compiler has a bug".  For user-facing errors, return a `CompileError`
/// instead.  For conditions that must be checked even in release builds, use
/// `mimi_assert!` instead.
///
/// # Example
///
/// ```ignore
/// mimi_debug_assert!(ty.is_resolved(), "type {:?} not resolved after pass 3", ty);
/// ```
#[macro_export]
macro_rules! mimi_debug_assert {
    ($cond:expr $(,)?) => {
        if cfg!(debug_assertions) && !$cond {
            eprintln!("[mimi-debug-assert] FAILED: {} at {}:{}",
                stringify!($cond), file!(), line!());
            debug_assert!($cond);
        }
    };
    ($cond:expr, $fmt:literal $(, $arg:expr)* $(,)?) => {
        if cfg!(debug_assertions) && !$cond {
            eprintln!("[mimi-debug-assert] FAILED: {} — {}",
                stringify!($cond), format!($fmt, $($arg),*));
            debug_assert!($cond, $fmt, $($arg)*);
        }
    };
}

/// Runtime invariant check (active in all build profiles).
///
/// Unlike `mimi_debug_assert!`, this guard is **not** elided in release builds.
/// It emits a warning via `eprintln!` when `$cond` is false, but does **not**
/// panic — use it for conditions that should be true but where crashing the
/// compiler is worse than continuing with degraded behaviour.
///
/// # When to use
///
/// Use this for low-overhead checks that are valuable even in release builds,
/// such as guardrails around FFI calls or memory-safety assertions in the
/// runtime library.
///
/// # Example
///
/// ```ignore
/// mimi_assert!(!ptr.is_null(), "heap pointer is null in free()");
/// ```
#[macro_export]
macro_rules! mimi_assert {
    ($cond:expr $(,)?) => {
        if !$cond {
            eprintln!("[mimi-assert] FAILED: {} at {}:{}",
                stringify!($cond), file!(), line!());
        }
    };
    ($cond:expr, $fmt:literal $(, $arg:expr)* $(,)?) => {
        if !$cond {
            eprintln!("[mimi-assert] FAILED: {} — {}",
                stringify!($cond), format!($fmt, $($arg),*));
        }
    };
}

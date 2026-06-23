#![allow(dead_code)]

//! FFI contract: describes how a single extern function signature crosses the
//! Mimi <-> C boundary.
//!
//! A contract is derived from the AST `ExternFunc` declaration.  It is consumed
//! by both the interpreter and the codegen backend so that the two FFI paths
//! behave identically: argument marshalling, lifetime extension, and return
//! value translation are driven by the same description.

use std::collections::HashSet;

use crate::ast::{CapMode, ExternFunc, Expr, Type};

/// Contract for one extern function.
#[derive(Debug, Clone)]
pub struct FfiContract {
    /// One contract per parameter, in declaration order.
    pub args: Vec<FfiArgContract>,
    /// Contract for the return value.
    pub ret: FfiRetContract,
    /// Precondition: must hold before the C function is called.
    pub requires: Option<Expr>,
    /// Postcondition: must hold after the C function returns.
    pub ensures: Option<Expr>,
    /// Whether to capture errno after the C call and map to Result.
    pub check_errno: bool,
}

/// How a single argument is translated from a Mimi value to a C ABI argument.
#[derive(Debug, Clone)]
pub enum FfiArgContract {
    /// Scalar integer or boolean passed by value.
    Int,
    /// 64-bit floating point passed by value.
    Float,
    /// A Mimi `string` passed as a temporary borrowed `char*`.
    StringBorrow,
    /// A Mimi `string` whose ownership is transferred to C (C must free).
    StringTransfer,
    /// A linear capability passed as an opaque handle.
    /// Preserves the `CapMode` (Borrow or Move) from the AST.
    Cap(CapMode),
    /// Raw immutable pointer `*T`.
    RawPtr(Box<Type>),
    /// Raw mutable pointer `*mut T`.
    RawPtrMut(Box<Type>),
    /// Shared ownership boundary handle `c_shared T`.
    CShared(Box<Type>),
    /// Borrowed read-only boundary handle `c_borrow T`.
    CBorrow(Box<Type>),
    /// Borrowed mutable boundary handle `c_borrow_mut T`.
    CBorrowMut(Box<Type>),
    /// JSON-serialized value: List, Record, Tuple etc. passed as a JSON string.
    /// The C side receives a `const char*` and must parse it.
    Json,
    /// A `#[repr(C)]` record passed as a C struct by value.
    /// The type name is stored so the interpreter/codegen can look up field
    /// definitions and build the correct ABI type descriptor.
    StructByValue(String),
    /// A C-compatible function pointer (callback).
    /// The Mimi closure is registered with the CallbackTable and a dynamically
    /// generated trampoline pointer is passed to C.
    Callback {
        /// Types of the callback's parameters.
        param_types: Vec<Type>,
        /// Return type of the callback.
        ret_type: Box<Type>,
    },
    /// A type that the type checker already rejects but which may still appear
    /// in runtime-only paths (e.g. tests that bypass `core::check`).  The
    /// wrapper reports this as an FFI safety error at call time.
    Unsupported(String),
}

/// How the return value is translated from the C ABI back to a Mimi value.
#[derive(Debug, Clone)]
pub enum FfiRetContract {
    /// No return value (`unit`).
    Unit,
    /// Scalar integer or boolean.
    Int,
    /// 64-bit floating point.
    Float,
    /// A null-terminated C string borrowed from C (Mimi does NOT free).
    String,
    /// A null-terminated C string whose ownership is transferred to Mimi.
    /// Mimi will free the C pointer with `libc::free` after reading.
    StringOwned,
    /// Raw immutable pointer `*T`.
    RawPtr(Box<Type>),
    /// Raw mutable pointer `*mut T`.
    RawPtrMut(Box<Type>),
    /// Shared ownership boundary handle `c_shared T`.
    CShared(Box<Type>),
    /// Borrowed read-only boundary handle `c_borrow T`.
    CBorrow(Box<Type>),
    /// Borrowed mutable boundary handle `c_borrow_mut T`.
    CBorrowMut(Box<Type>),
    /// JSON-serialized return value: List, Record, Tuple etc. returned as a
    /// C string (Mimi frees the pointer after reading).
    Json,
    /// A `#[repr(C)]` record returned as a C struct by value.
    StructByValue(String),
    /// A return type that the type checker rejects but which may be reached at
    /// runtime in tests that bypass `core::check`.
    Unsupported(String),
}

/// Functions whose return values (i32/i64) conventionally indicate error via
/// negative values / -1, triggering automatic errno capture after the C call.
/// Used by both `FfiContract` and the Python binding generator.
pub const ERRNO_CHECK_FUNC_NAMES: &[&str] = &[
    "errno", "strerror", "perror",
    "open", "openat", "creat", "fopen", "fdopen",
    "read", "write", "pread", "pwrite", "readv", "writev",
    "socket", "connect", "bind", "listen", "accept", "accept4",
    "send", "recv", "sendto", "recvfrom", "sendmsg", "recvmsg",
    "close", "shutdown", "dup", "dup2", "dup3",
    "fcntl", "ioctl", "poll", "select", "epoll_create", "epoll_ctl", "epoll_wait",
    "fork", "execve", "wait", "waitpid", "waitid",
    "kill", "raise", "signal", "sigaction", "sigprocmask",
    "pipe", "pipe2", "mkfifo", "socketpair",
    "getaddrinfo", "freeaddrinfo", "getnameinfo",
    "gethostbyname", "gethostbyaddr",
    "dlopen", "dlsym", "dlerror", "dlclose",
    "mmap", "munmap", "mprotect", "msync",
    "opendir", "readdir", "closedir",
    "stat", "fstat", "lstat", "access", "chmod", "chown",
    "link", "unlink", "rename", "symlink", "mkdir", "rmdir",
    "mount", "umount", "chdir", "fchdir", "getcwd",
    "setjmp", "longjmp", "sigsetjmp", "siglongjmp",
    "time", "ctime", "localtime", "gmtime",
    "strtol", "strtoll", "strtoul", "strtoull", "atoi", "atol",
    "malloc", "calloc", "realloc", "posix_memalign",
    "pthread_create", "pthread_join", "pthread_mutex_lock", "pthread_mutex_unlock",
    "sem_init", "sem_wait", "sem_post", "sem_destroy",
    "mq_open", "mq_send", "mq_receive", "mq_close", "mq_unlink",
    "clock_gettime", "clock_settime", "timer_create", "timer_settime",
    "getenv", "setenv", "unsetenv", "putenv",
    "system", "popen", "pclose", "execl", "execle", "execlp", "execv", "execvp",
    "realpath", "canonicalize_file_name",
    "tempnam", "tmpfile", "mkstemp", "mkdtemp",
    "getopt", "getopt_long", "getopt_long_only",
];

impl FfiContract {
    /// Build a contract from an extern function declaration.
    ///
    /// The caller is responsible for ensuring the declaration has already been
    /// validated by the type checker (`is_valid_extern_type`).  This function
    /// panics on unexpected types so that contract bugs surface early.
    pub fn from_extern(func: &ExternFunc) -> Self {
        Self::from_extern_with_caps(func, &HashSet::new(), &HashSet::new())
    }

    /// Build a contract from an extern function declaration, with knowledge of
    /// which type names refer to declared capabilities, which refer to records,
    /// and which records have #[repr(C)].
    pub fn from_extern_with_caps(func: &ExternFunc, cap_names: &HashSet<String>, record_type_names: &HashSet<String>) -> Self {
        Self::from_extern_with_caps_repr(func, cap_names, record_type_names, &HashSet::new())
    }

    /// Like `from_extern_with_caps`, but also takes repr_c_record_names so that
    /// #[repr(C)] records are distinguished from plain records.  Codegen passes
    /// these directly by value (LLVM struct); the interpreter still serializes
    /// them to JSON until the libffi struct-by-value path is implemented.
    pub fn from_extern_with_caps_repr(
        func: &ExternFunc,
        cap_names: &HashSet<String>,
        record_type_names: &HashSet<String>,
        repr_c_record_names: &HashSet<String>,
    ) -> Self {
        let args = func
            .params
            .iter()
            .map(|p| {
                if let Some(mode) = p.cap_mode {
                    FfiArgContract::Cap(mode)
                } else {
                    FfiArgContract::from_type_with_caps(&p.ty, cap_names, record_type_names, repr_c_record_names)
                }
            })
            .collect();
        let ret = func
            .ret
            .as_ref()
            .map(|ty| FfiRetContract::from_type_with_caps(ty, cap_names, record_type_names, repr_c_record_names))
            .unwrap_or(FfiRetContract::Unit);

        // Auto-enable errno checking if return type is Result-like
        // (convention: negative return values indicate errors).
        // Uses exact function name matching (not `contains`) to avoid false
        // positives on wrapper functions like `my_open_wrapper`.
        let fname: &str = &func.name;
        let check_errno = matches!(&func.ret, Some(Type::Name(name, _)) if name == "i32" || name == "i64")
            && ERRNO_CHECK_FUNC_NAMES.contains(&fname);

        Self {
            args,
            ret,
            requires: func.requires.clone(),
            ensures: func.ensures.clone(),
            check_errno,
        }
    }

    /// Create a contract with explicit errno checking
    pub fn from_extern_with_errno(func: &ExternFunc) -> Self {
        let mut contract = Self::from_extern(func);
        contract.check_errno = true;
        contract
    }
}

impl FfiArgContract {
    fn from_type_with_caps(ty: &Type, cap_names: &HashSet<String>, record_type_names: &HashSet<String>, repr_c_record_names: &HashSet<String>) -> Self {
        match ty {
            Type::Name(name, _) => {
                if cap_names.contains(name.as_str()) {
                    return FfiArgContract::Cap(CapMode::Borrow);
                }
                match name.as_str() {
                    "i32" | "i64" | "bool" => FfiArgContract::Int,
                    "f64" => FfiArgContract::Float,
                    "string" => FfiArgContract::StringBorrow,
                    "unit" => FfiArgContract::Int,
                    "List" => FfiArgContract::Json,
                    other => {
                        if record_type_names.contains(other) {
                            if repr_c_record_names.contains(other) {
                                // #[repr(C)] record: pass as C struct by value
                                FfiArgContract::StructByValue(other.to_string())
                            } else {
                                FfiArgContract::Json
                            }
                        } else {
                            FfiArgContract::Unsupported(other.to_string())
                        }
                    }
                }
            }
            Type::Cap(_) => FfiArgContract::Cap(CapMode::Move),
            Type::RawPtr(inner) => FfiArgContract::RawPtr(inner.clone()),
            Type::RawPtrMut(inner) => FfiArgContract::RawPtrMut(inner.clone()),
            Type::CShared(inner) => FfiArgContract::CShared(inner.clone()),
            Type::CBorrow(inner) => FfiArgContract::CBorrow(inner.clone()),
            Type::CBorrowMut(inner) => FfiArgContract::CBorrowMut(inner.clone()),
            Type::RawString => FfiArgContract::StringTransfer,
            Type::ExternFunc(param_types, ret_type) | Type::Func(param_types, ret_type) => {
                FfiArgContract::Callback { param_types: param_types.clone(), ret_type: ret_type.clone() }
            }
            Type::CBuffer(inner) => FfiArgContract::RawPtr(inner.clone()),
            Type::Tuple(_) => FfiArgContract::Json,
            other => FfiArgContract::Unsupported(format!("{:?}", other)),
        }
    }
}

impl FfiRetContract {
    fn from_type_with_caps(ty: &Type, _cap_names: &HashSet<String>, record_type_names: &HashSet<String>, repr_c_record_names: &HashSet<String>) -> Self {
        match ty {
            Type::Name(name, _) => match name.as_str() {
                "i32" | "i64" | "bool" => FfiRetContract::Int,
                "f64" => FfiRetContract::Float,
                "string" => FfiRetContract::String,
                "unit" => FfiRetContract::Unit,
                "List" => FfiRetContract::Json,
                other => {
                    if record_type_names.contains(other) {
                        if repr_c_record_names.contains(other) {
                            FfiRetContract::StructByValue(other.to_string())
                        } else {
                            FfiRetContract::Json
                        }
                    } else {
                        FfiRetContract::Unsupported(other.to_string())
                    }
                }
            },
            Type::RawPtr(inner) => FfiRetContract::RawPtr(inner.clone()),
            Type::RawPtrMut(inner) => FfiRetContract::RawPtrMut(inner.clone()),
            Type::CShared(inner) => FfiRetContract::CShared(inner.clone()),
            Type::CBorrow(inner) => FfiRetContract::CBorrow(inner.clone()),
            Type::CBorrowMut(inner) => FfiRetContract::CBorrowMut(inner.clone()),
            Type::RawString => FfiRetContract::StringOwned,
            Type::ExternFunc(_, _) => {
                FfiRetContract::RawPtr(Box::new(Type::Name("unit".to_string(), vec![])))
            }
            Type::CBuffer(inner) => FfiRetContract::RawPtr(inner.clone()),
            Type::Tuple(_) => FfiRetContract::Json,
            other => FfiRetContract::Unsupported(format!("{:?}", other)),
        }
    }
}

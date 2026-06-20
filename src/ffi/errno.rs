use std::fmt;
use thiserror::Error;

/// Structured POSIX errno codes for FFI error handling.
///
/// Replaces the old `Err(format!("FFI errno: {} (code {})", name, code))`
/// pattern with a typed enum that callers can match on.
///
/// # Example
///
/// ```rust
/// match errno {
///     Errno::ENOENT => { /* file not found */ }
///     Errno::EACCES => { /* permission denied */ }
///     _ => { /* other */ }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Errno {
    // === POSIX error codes 1–34 (Linux errno-base.h) ===
    #[error("EPERM (code 1): Operation not permitted")]
    EPERM,
    #[error("ENOENT (code 2): No such file or directory")]
    ENOENT,
    #[error("ESRCH (code 3): No such process")]
    ESRCH,
    #[error("EINTR (code 4): Interrupted system call")]
    EINTR,
    #[error("EIO (code 5): I/O error")]
    EIO,
    #[error("ENXIO (code 6): No such device or address")]
    ENXIO,
    #[error("E2BIG (code 7): Argument list too long")]
    E2BIG,
    #[error("ENOEXEC (code 8): Exec format error")]
    ENOEXEC,
    #[error("EBADF (code 9): Bad file number")]
    EBADF,
    #[error("ECHILD (code 10): No child processes")]
    ECHILD,
    #[error("EAGAIN (code 11): Try again")]
    EAGAIN,
    #[error("ENOMEM (code 12): Out of memory")]
    ENOMEM,
    #[error("EACCES (code 13): Permission denied")]
    EACCES,
    #[error("EFAULT (code 14): Bad address")]
    EFAULT,
    #[error("ENOTBLK (code 15): Block device required")]
    ENOTBLK,
    #[error("EBUSY (code 16): Device or resource busy")]
    EBUSY,
    #[error("EEXIST (code 17): File exists")]
    EEXIST,
    #[error("EXDEV (code 18): Cross-device link")]
    EXDEV,
    #[error("ENODEV (code 19): No such device")]
    ENODEV,
    #[error("ENOTDIR (code 20): Not a directory")]
    ENOTDIR,
    #[error("EISDIR (code 21): Is a directory")]
    EISDIR,
    #[error("EINVAL (code 22): Invalid argument")]
    EINVAL,
    #[error("ENFILE (code 23): File table overflow")]
    ENFILE,
    #[error("EMFILE (code 24): Too many open files")]
    EMFILE,
    #[error("ENOTTY (code 25): Not a typewriter")]
    ENOTTY,
    #[error("ETXTBSY (code 26): Text file busy")]
    ETXTBSY,
    #[error("EFBIG (code 27): File too large")]
    EFBIG,
    #[error("ENOSPC (code 28): No space left on device")]
    ENOSPC,
    #[error("ESPIPE (code 29): Illegal seek")]
    ESPIPE,
    #[error("EROFS (code 30): Read-only file system")]
    EROFS,
    #[error("EMLINK (code 31): Too many links")]
    EMLINK,
    #[error("EPIPE (code 32): Broken pipe")]
    EPIPE,
    #[error("EDOM (code 33): Math argument out of domain of func")]
    EDOM,
    #[error("ERANGE (code 34): Math result not representable")]
    ERANGE,

    // === POSIX error codes 35–133 (Linux asm-generic/errno.h) ===
    #[error("EDEADLK (code 35): Resource deadlock would occur")]
    EDEADLK,
    #[error("ENAMETOOLONG (code 36): File name too long")]
    ENAMETOOLONG,
    #[error("ENOLCK (code 37): No record locks available")]
    ENOLCK,
    #[error("ENOSYS (code 38): Invalid system call number")]
    ENOSYS,
    #[error("ENOTEMPTY (code 39): Directory not empty")]
    ENOTEMPTY,
    #[error("ELOOP (code 40): Too many symbolic links")]
    ELOOP,
    #[error("ENOMSG (code 42): No message of desired type")]
    ENOMSG,
    #[error("EIDRM (code 43): Identifier removed")]
    EIDRM,
    #[error("ECHRNG (code 44): Channel number out of range")]
    ECHRNG,
    #[error("EL2NSYNC (code 45): Level 2 not synchronized")]
    EL2NSYNC,
    #[error("EL3HLT (code 46): Level 3 halted")]
    EL3HLT,
    #[error("EL3RST (code 47): Level 3 reset")]
    EL3RST,
    #[error("ELNRNG (code 48): Link number out of range")]
    ELNRNG,
    #[error("EUNATCH (code 49): Protocol driver not attached")]
    EUNATCH,
    #[error("ENOCSI (code 50): No CSI structure available")]
    ENOCSI,
    #[error("EL2HLT (code 51): Level 2 halted")]
    EL2HLT,
    #[error("EBADE (code 52): Invalid exchange")]
    EBADE,
    #[error("EBADR (code 53): Invalid request descriptor")]
    EBADR,
    #[error("EXFULL (code 54): Exchange full")]
    EXFULL,
    #[error("ENOANO (code 55): No anode")]
    ENOANO,
    #[error("EBADRQC (code 56): Invalid request code")]
    EBADRQC,
    #[error("EBADSLT (code 57): Invalid slot")]
    EBADSLT,
    #[error("EBFONT (code 59): Bad font file format")]
    EBFONT,
    #[error("ENOSTR (code 60): Device not a stream")]
    ENOSTR,
    #[error("ENODATA (code 61): No data available")]
    ENODATA,
    #[error("ETIME (code 62): Timer expired")]
    ETIME,
    #[error("ENOSR (code 63): Out of streams resources")]
    ENOSR,
    #[error("ENONET (code 64): Machine is not on the network")]
    ENONET,
    #[error("ENOPKG (code 65): Package not installed")]
    ENOPKG,
    #[error("EREMOTE (code 66): Object is remote")]
    EREMOTE,
    #[error("ENOLINK (code 67): Link has been severed")]
    ENOLINK,
    #[error("EADV (code 68): Advertise error")]
    EADV,
    #[error("ESRMNT (code 69): Srmount error")]
    ESRMNT,
    #[error("ECOMM (code 70): Communication error on send")]
    ECOMM,
    #[error("EPROTO (code 71): Protocol error")]
    EPROTO,
    #[error("EMULTIHOP (code 72): Multihop attempted")]
    EMULTIHOP,
    #[error("EDOTDOT (code 73): RFS specific error")]
    EDOTDOT,
    #[error("EBADMSG (code 74): Not a data message")]
    EBADMSG,
    #[error("EOVERFLOW (code 75): Value too large for defined data type")]
    EOVERFLOW,
    #[error("ENOTUNIQ (code 76): Name not unique on network")]
    ENOTUNIQ,
    #[error("EBADFD (code 77): File descriptor in bad state")]
    EBADFD,
    #[error("EREMCHG (code 78): Remote address changed")]
    EREMCHG,
    #[error("ELIBACC (code 79): Can not access a needed shared library")]
    ELIBACC,
    #[error("ELIBBAD (code 80): Accessing a corrupted shared library")]
    ELIBBAD,
    #[error("ELIBSCN (code 81): .lib section in a.out corrupted")]
    ELIBSCN,
    #[error("ELIBMAX (code 82): Attempting to link in too many shared libraries")]
    ELIBMAX,
    #[error("ELIBEXEC (code 83): Cannot exec a shared library directly")]
    ELIBEXEC,
    #[error("EILSEQ (code 84): Illegal byte sequence")]
    EILSEQ,
    #[error("ERESTART (code 85): Interrupted system call should be restarted")]
    ERESTART,
    #[error("ESTRPIPE (code 86): Streams pipe error")]
    ESTRPIPE,
    #[error("EUSERS (code 87): Too many users")]
    EUSERS,
    #[error("ENOTSOCK (code 88): Socket operation on non-socket")]
    ENOTSOCK,
    #[error("EDESTADDRREQ (code 89): Destination address required")]
    EDESTADDRREQ,
    #[error("EMSGSIZE (code 90): Message too long")]
    EMSGSIZE,
    #[error("EPROTOTYPE (code 91): Protocol wrong type for socket")]
    EPROTOTYPE,
    #[error("ENOPROTOOPT (code 92): Protocol not available")]
    ENOPROTOOPT,
    #[error("EPROTONOSUPPORT (code 93): Protocol not supported")]
    EPROTONOSUPPORT,
    #[error("ESOCKTNOSUPPORT (code 94): Socket type not supported")]
    ESOCKTNOSUPPORT,
    #[error("EOPNOTSUPP (code 95): Operation not supported on transport endpoint")]
    EOPNOTSUPP,
    #[error("EPFNOSUPPORT (code 96): Protocol family not supported")]
    EPFNOSUPPORT,
    #[error("EAFNOSUPPORT (code 97): Address family not supported by protocol")]
    EAFNOSUPPORT,
    #[error("EADDRINUSE (code 98): Address already in use")]
    EADDRINUSE,
    #[error("EADDRNOTAVAIL (code 99): Cannot assign requested address")]
    EADDRNOTAVAIL,
    #[error("ENETDOWN (code 100): Network is down")]
    ENETDOWN,
    #[error("ENETUNREACH (code 101): Network is unreachable")]
    ENETUNREACH,
    #[error("ENETRESET (code 102): Network dropped connection because of reset")]
    ENETRESET,
    #[error("ECONNABORTED (code 103): Software caused connection abort")]
    ECONNABORTED,
    #[error("ECONNRESET (code 104): Connection reset by peer")]
    ECONNRESET,
    #[error("ENOBUFS (code 105): No buffer space available")]
    ENOBUFS,
    #[error("EISCONN (code 106): Transport endpoint is already connected")]
    EISCONN,
    #[error("ENOTCONN (code 107): Transport endpoint is not connected")]
    ENOTCONN,
    #[error("ESHUTDOWN (code 108): Cannot send after transport endpoint shutdown")]
    ESHUTDOWN,
    #[error("ETOOMANYREFS (code 109): Too many references: cannot splice")]
    ETOOMANYREFS,
    #[error("ETIMEDOUT (code 110): Connection timed out")]
    ETIMEDOUT,
    #[error("ECONNREFUSED (code 111): Connection refused")]
    ECONNREFUSED,
    #[error("EHOSTDOWN (code 112): Host is down")]
    EHOSTDOWN,
    #[error("EHOSTUNREACH (code 113): No route to host")]
    EHOSTUNREACH,
    #[error("EALREADY (code 114): Operation already in progress")]
    EALREADY,
    #[error("EINPROGRESS (code 115): Operation now in progress")]
    EINPROGRESS,
    #[error("ESTALE (code 116): Stale file handle")]
    ESTALE,
    #[error("EUCLEAN (code 117): Structure needs cleaning")]
    EUCLEAN,
    #[error("ENOTNAM (code 118): Not a XENIX named type file")]
    ENOTNAM,
    #[error("ENAVAIL (code 119): No XENIX semaphores available")]
    ENAVAIL,
    #[error("EISNAM (code 120): Is a named type file")]
    EISNAM,
    #[error("EREMOTEIO (code 121): Remote I/O error")]
    EREMOTEIO,
    #[error("EDQUOT (code 122): Quota exceeded")]
    EDQUOT,
    #[error("ENOMEDIUM (code 123): No medium found")]
    ENOMEDIUM,
    #[error("EMEDIUMTYPE (code 124): Wrong medium type")]
    EMEDIUMTYPE,
    #[error("ECANCELED (code 125): Operation canceled")]
    ECANCELED,
    #[error("ENOKEY (code 126): Required key not available")]
    ENOKEY,
    #[error("EKEYEXPIRED (code 127): Key has expired")]
    EKEYEXPIRED,
    #[error("EKEYREVOKED (code 128): Key has been revoked")]
    EKEYREVOKED,
    #[error("EKEYREJECTED (code 129): Key was rejected by service")]
    EKEYREJECTED,
    #[error("EOWNERDEAD (code 130): Owner died")]
    EOWNERDEAD,
    #[error("ENOTRECOVERABLE (code 131): State not recoverable")]
    ENOTRECOVERABLE,
    #[error("ERFKILL (code 132): Operation not possible due to RF-kill")]
    ERFKILL,
    #[error("EHWPOISON (code 133): Memory page has hardware error")]
    EHWPOISON,

    /// Unknown errno code (not in the POSIX 1–133 range).
    #[error("Unknown errno (code {0})")]
    Unknown(i32),

    /// Unknown errno code with a human-readable name from `strerror`.
    #[error("Unknown errno (code {0}): {1}")]
    UnknownWithName(i32, String),

    /// Non-errno FFI wrapper error (argument validation, library loading, etc.).
    #[error("FFI wrapper: {0}")]
    Generic(String),
}

impl Errno {
    /// Map a raw POSIX errno integer to the corresponding `Errno` variant.
    pub fn from_code(code: i32) -> Self {
        match code {
            1 => Self::EPERM,
            2 => Self::ENOENT,
            3 => Self::ESRCH,
            4 => Self::EINTR,
            5 => Self::EIO,
            6 => Self::ENXIO,
            7 => Self::E2BIG,
            8 => Self::ENOEXEC,
            9 => Self::EBADF,
            10 => Self::ECHILD,
            11 => Self::EAGAIN,
            12 => Self::ENOMEM,
            13 => Self::EACCES,
            14 => Self::EFAULT,
            15 => Self::ENOTBLK,
            16 => Self::EBUSY,
            17 => Self::EEXIST,
            18 => Self::EXDEV,
            19 => Self::ENODEV,
            20 => Self::ENOTDIR,
            21 => Self::EISDIR,
            22 => Self::EINVAL,
            23 => Self::ENFILE,
            24 => Self::EMFILE,
            25 => Self::ENOTTY,
            26 => Self::ETXTBSY,
            27 => Self::EFBIG,
            28 => Self::ENOSPC,
            29 => Self::ESPIPE,
            30 => Self::EROFS,
            31 => Self::EMLINK,
            32 => Self::EPIPE,
            33 => Self::EDOM,
            34 => Self::ERANGE,
            35 => Self::EDEADLK,
            36 => Self::ENAMETOOLONG,
            37 => Self::ENOLCK,
            38 => Self::ENOSYS,
            39 => Self::ENOTEMPTY,
            40 => Self::ELOOP,
            42 => Self::ENOMSG,
            43 => Self::EIDRM,
            44 => Self::ECHRNG,
            45 => Self::EL2NSYNC,
            46 => Self::EL3HLT,
            47 => Self::EL3RST,
            48 => Self::ELNRNG,
            49 => Self::EUNATCH,
            50 => Self::ENOCSI,
            51 => Self::EL2HLT,
            52 => Self::EBADE,
            53 => Self::EBADR,
            54 => Self::EXFULL,
            55 => Self::ENOANO,
            56 => Self::EBADRQC,
            57 => Self::EBADSLT,
            59 => Self::EBFONT,
            60 => Self::ENOSTR,
            61 => Self::ENODATA,
            62 => Self::ETIME,
            63 => Self::ENOSR,
            64 => Self::ENONET,
            65 => Self::ENOPKG,
            66 => Self::EREMOTE,
            67 => Self::ENOLINK,
            68 => Self::EADV,
            69 => Self::ESRMNT,
            70 => Self::ECOMM,
            71 => Self::EPROTO,
            72 => Self::EMULTIHOP,
            73 => Self::EDOTDOT,
            74 => Self::EBADMSG,
            75 => Self::EOVERFLOW,
            76 => Self::ENOTUNIQ,
            77 => Self::EBADFD,
            78 => Self::EREMCHG,
            79 => Self::ELIBACC,
            80 => Self::ELIBBAD,
            81 => Self::ELIBSCN,
            82 => Self::ELIBMAX,
            83 => Self::ELIBEXEC,
            84 => Self::EILSEQ,
            85 => Self::ERESTART,
            86 => Self::ESTRPIPE,
            87 => Self::EUSERS,
            88 => Self::ENOTSOCK,
            89 => Self::EDESTADDRREQ,
            90 => Self::EMSGSIZE,
            91 => Self::EPROTOTYPE,
            92 => Self::ENOPROTOOPT,
            93 => Self::EPROTONOSUPPORT,
            94 => Self::ESOCKTNOSUPPORT,
            95 => Self::EOPNOTSUPP,
            96 => Self::EPFNOSUPPORT,
            97 => Self::EAFNOSUPPORT,
            98 => Self::EADDRINUSE,
            99 => Self::EADDRNOTAVAIL,
            100 => Self::ENETDOWN,
            101 => Self::ENETUNREACH,
            102 => Self::ENETRESET,
            103 => Self::ECONNABORTED,
            104 => Self::ECONNRESET,
            105 => Self::ENOBUFS,
            106 => Self::EISCONN,
            107 => Self::ENOTCONN,
            108 => Self::ESHUTDOWN,
            109 => Self::ETOOMANYREFS,
            110 => Self::ETIMEDOUT,
            111 => Self::ECONNREFUSED,
            112 => Self::EHOSTDOWN,
            113 => Self::EHOSTUNREACH,
            114 => Self::EALREADY,
            115 => Self::EINPROGRESS,
            116 => Self::ESTALE,
            117 => Self::EUCLEAN,
            118 => Self::ENOTNAM,
            119 => Self::ENAVAIL,
            120 => Self::EISNAM,
            121 => Self::EREMOTEIO,
            122 => Self::EDQUOT,
            123 => Self::ENOMEDIUM,
            124 => Self::EMEDIUMTYPE,
            125 => Self::ECANCELED,
            126 => Self::ENOKEY,
            127 => Self::EKEYEXPIRED,
            128 => Self::EKEYREVOKED,
            129 => Self::EKEYREJECTED,
            130 => Self::EOWNERDEAD,
            131 => Self::ENOTRECOVERABLE,
            132 => Self::ERFKILL,
            133 => Self::EHWPOISON,
            _ => {
                let name = unsafe {
                    let c_str = libc::strerror(code);
                    if !c_str.is_null() {
                        std::ffi::CStr::from_ptr(c_str).to_string_lossy().into_owned()
                    } else {
                        format!("Unknown (code {})", code)
                    }
                };
                Self::UnknownWithName(code, name)
            }
        }
    }

    /// Return the numeric POSIX errno code, or 0 for `Generic`.
    pub fn code(&self) -> i32 {
        match self {
            Self::EPERM => 1,
            Self::ENOENT => 2,
            Self::ESRCH => 3,
            Self::EINTR => 4,
            Self::EIO => 5,
            Self::ENXIO => 6,
            Self::E2BIG => 7,
            Self::ENOEXEC => 8,
            Self::EBADF => 9,
            Self::ECHILD => 10,
            Self::EAGAIN => 11,
            Self::ENOMEM => 12,
            Self::EACCES => 13,
            Self::EFAULT => 14,
            Self::ENOTBLK => 15,
            Self::EBUSY => 16,
            Self::EEXIST => 17,
            Self::EXDEV => 18,
            Self::ENODEV => 19,
            Self::ENOTDIR => 20,
            Self::EISDIR => 21,
            Self::EINVAL => 22,
            Self::ENFILE => 23,
            Self::EMFILE => 24,
            Self::ENOTTY => 25,
            Self::ETXTBSY => 26,
            Self::EFBIG => 27,
            Self::ENOSPC => 28,
            Self::ESPIPE => 29,
            Self::EROFS => 30,
            Self::EMLINK => 31,
            Self::EPIPE => 32,
            Self::EDOM => 33,
            Self::ERANGE => 34,
            Self::EDEADLK => 35,
            Self::ENAMETOOLONG => 36,
            Self::ENOLCK => 37,
            Self::ENOSYS => 38,
            Self::ENOTEMPTY => 39,
            Self::ELOOP => 40,
            Self::ENOMSG => 42,
            Self::EIDRM => 43,
            Self::ECHRNG => 44,
            Self::EL2NSYNC => 45,
            Self::EL3HLT => 46,
            Self::EL3RST => 47,
            Self::ELNRNG => 48,
            Self::EUNATCH => 49,
            Self::ENOCSI => 50,
            Self::EL2HLT => 51,
            Self::EBADE => 52,
            Self::EBADR => 53,
            Self::EXFULL => 54,
            Self::ENOANO => 55,
            Self::EBADRQC => 56,
            Self::EBADSLT => 57,
            Self::EBFONT => 59,
            Self::ENOSTR => 60,
            Self::ENODATA => 61,
            Self::ETIME => 62,
            Self::ENOSR => 63,
            Self::ENONET => 64,
            Self::ENOPKG => 65,
            Self::EREMOTE => 66,
            Self::ENOLINK => 67,
            Self::EADV => 68,
            Self::ESRMNT => 69,
            Self::ECOMM => 70,
            Self::EPROTO => 71,
            Self::EMULTIHOP => 72,
            Self::EDOTDOT => 73,
            Self::EBADMSG => 74,
            Self::EOVERFLOW => 75,
            Self::ENOTUNIQ => 76,
            Self::EBADFD => 77,
            Self::EREMCHG => 78,
            Self::ELIBACC => 79,
            Self::ELIBBAD => 80,
            Self::ELIBSCN => 81,
            Self::ELIBMAX => 82,
            Self::ELIBEXEC => 83,
            Self::EILSEQ => 84,
            Self::ERESTART => 85,
            Self::ESTRPIPE => 86,
            Self::EUSERS => 87,
            Self::ENOTSOCK => 88,
            Self::EDESTADDRREQ => 89,
            Self::EMSGSIZE => 90,
            Self::EPROTOTYPE => 91,
            Self::ENOPROTOOPT => 92,
            Self::EPROTONOSUPPORT => 93,
            Self::ESOCKTNOSUPPORT => 94,
            Self::EOPNOTSUPP => 95,
            Self::EPFNOSUPPORT => 96,
            Self::EAFNOSUPPORT => 97,
            Self::EADDRINUSE => 98,
            Self::EADDRNOTAVAIL => 99,
            Self::ENETDOWN => 100,
            Self::ENETUNREACH => 101,
            Self::ENETRESET => 102,
            Self::ECONNABORTED => 103,
            Self::ECONNRESET => 104,
            Self::ENOBUFS => 105,
            Self::EISCONN => 106,
            Self::ENOTCONN => 107,
            Self::ESHUTDOWN => 108,
            Self::ETOOMANYREFS => 109,
            Self::ETIMEDOUT => 110,
            Self::ECONNREFUSED => 111,
            Self::EHOSTDOWN => 112,
            Self::EHOSTUNREACH => 113,
            Self::EALREADY => 114,
            Self::EINPROGRESS => 115,
            Self::ESTALE => 116,
            Self::EUCLEAN => 117,
            Self::ENOTNAM => 118,
            Self::ENAVAIL => 119,
            Self::EISNAM => 120,
            Self::EREMOTEIO => 121,
            Self::EDQUOT => 122,
            Self::ENOMEDIUM => 123,
            Self::EMEDIUMTYPE => 124,
            Self::ECANCELED => 125,
            Self::ENOKEY => 126,
            Self::EKEYEXPIRED => 127,
            Self::EKEYREVOKED => 128,
            Self::EKEYREJECTED => 129,
            Self::EOWNERDEAD => 130,
            Self::ENOTRECOVERABLE => 131,
            Self::ERFKILL => 132,
            Self::EHWPOISON => 133,
            Self::Unknown(c) | Self::UnknownWithName(c, _) => *c,
            Self::Generic(_) => 0,
        }
    }

    /// Returns `true` if this is a POSIX errno variant (not `Generic` or `Unknown`).
    pub fn is_posix_errno(&self) -> bool {
        !matches!(self, Self::Generic(_) | Self::Unknown(_) | Self::UnknownWithName(_, _))
    }
}

impl From<String> for Errno {
    fn from(msg: String) -> Self {
        Self::Generic(msg)
    }
}

impl From<&str> for Errno {
    fn from(msg: &str) -> Self {
        Self::Generic(msg.to_string())
    }
}

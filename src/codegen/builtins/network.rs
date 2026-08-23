use super::super::call_try_basic_value;
use super::super::CallSiteValueExt;
use super::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    // ── Audit wave2 (red line §1.4, ruling §1.1.1): net NULL contract ──
    //
    // Runtime `mimi_recv` / `mimi_http_get` / `mimi_http_post` return NULL
    // on error and do NOT abort (runtime/net.rs). The VM builtins are clean:
    // they raise `Err(InterpError)` → the program fails loud with the error
    // message (interp/bytecode/builtins/net.rs). Old codegen packed
    // {NULL, 0} into a Mimi string (recv) or substituted an empty string
    // (http), turning real network errors (ECONNRESET, refused, …) into
    // Ok(dangling/empty string) on the compiled path — the exact blind spot
    // that shipped in Wave-1's stdlib net change. Fix: NULL → trap with a
    // VM-shaped message. Runtime `mimi_recv` now returns a non-NULL empty
    // string for the n == 0 / EOF case, so compiled `recv` preserves the
    // VM's Ok("") EOF semantics while still trapping on real errors.
    // The `Err` shape contract (deliverable for STDLIB's codegen-side
    // assertions):
    //   recv:     "recv: buf_size must be positive"            (buf_size<=0)
    //   recv:     "recv() failed: fd=%ld, buf_size=%ld (network error)"
    //   http_get: "http_get: request failed"
    //   http_post:"http_post: request failed"

    /// Trap with a static message via `mimi_runtime_abort` (noreturn), then
    /// mark the block unreachable. Caller arranges control flow around it.
    fn emit_net_trap(&self, message: &str, label: &str) -> MimiResult<()> {
        let abort_fn = self.get_or_declare_abort_fn();
        let msg = self
            .builder
            .build_global_string_ptr(message, &format!("{}_msg", label))
            .map_err(|e| format!("global string error: {}", e))?;
        self.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(msg.as_pointer_value())],
            &format!("{}_abort", label),
        )?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| format!("unreachable error: {}", e))?;
        Ok(())
    }

    /// Trap with a message formatted from two i64 values (recv fd/buf_size).
    fn emit_net_trap_2i(
        &self,
        fmt_str: &str,
        a: inkwell::values::IntValue<'ctx>,
        b: inkwell::values::IntValue<'ctx>,
        label: &str,
    ) -> MimiResult<()> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let buf = self.build_alloca(i64_ty.array_type(16), &format!("{}_msg", label))?;
        let fmt_global = self
            .builder
            .build_global_string_ptr(fmt_str, &format!("{}_fmt", label))
            .map_err(|e| format!("global string error: {}", e))?;
        let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
            self.module.add_function(
                "snprintf",
                i32_ty.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(i64_ty),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                    ],
                    true,
                ),
                Some(inkwell::module::Linkage::External),
            )
        });
        self.builder
            .build_call(
                snprintf_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(128, false)),
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                    BasicMetadataValueEnum::IntValue(a),
                    BasicMetadataValueEnum::IntValue(b),
                ],
                &format!("{}_snprintf", label),
            )
            .map_err(|e| format!("snprintf error: {}", e))?;
        let abort_fn = self.get_or_declare_abort_fn();
        self.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(buf)],
            &format!("{}_abort", label),
        )?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| format!("unreachable error: {}", e))?;
        Ok(())
    }

    pub(super) fn compile_socket(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 3 {
            return Err(CompileError::WrongArgCount(
                "socket expects 3 arguments".to_string(),
            ));
        }
        let domain = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "socket: domain must be i32".to_string(),
                ))
            }
        };
        let type_ = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "socket: type must be i32".to_string(),
                ))
            }
        };
        let protocol = match args[2] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "socket: protocol must be i32".to_string(),
                ))
            }
        };
        let func = self
            .module
            .get_function("mimi_socket")
            .ok_or("mimi_socket not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(domain),
                    BasicMetadataValueEnum::IntValue(type_),
                    BasicMetadataValueEnum::IntValue(protocol),
                ],
                "socket_call",
            )
            .map_err(|e| format!("socket error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_socket returned void")?)
    }

    pub(super) fn compile_connect(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 3 {
            return Err(CompileError::WrongArgCount(
                "connect expects 3 arguments (fd, host, port)".to_string(),
            ));
        }
        let fd = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "connect: fd must be i32".to_string(),
                ))
            }
        };
        let host_ptr = self.extract_raw_str_ptr(&args[1])?;
        let port = match args[2] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "connect: port must be i32".to_string(),
                ))
            }
        };
        let func = self
            .module
            .get_function("mimi_connect")
            .ok_or("mimi_connect not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(fd),
                    BasicMetadataValueEnum::PointerValue(host_ptr),
                    BasicMetadataValueEnum::IntValue(port),
                ],
                "connect_call",
            )
            .map_err(|e| format!("connect error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_connect returned void")?)
    }

    pub(super) fn compile_bind(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "bind expects 2 arguments (fd, port)".to_string(),
            ));
        }
        let fd = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "bind: fd must be i32".to_string(),
                ))
            }
        };
        let port = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "bind: port must be i32".to_string(),
                ))
            }
        };
        // VM parity: reject ports outside the u16 range before the C socket
        // bind, instead of silently truncating (batch5-03 P2-2).
        let i64_ty = self.context.i64_type();
        let port_64 = if port.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(port, i64_ty, "bind_port_sext")
                .map_err(|e| format!("bind port sext error: {}", e))?
        } else {
            port
        };
        let bad_port = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                port_64,
                i64_ty.const_int(0, false),
                "bind_port_lt0",
            )
            .map_err(|e| format!("bind port lt0 error: {}", e))?;
        let port_gt_max = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGT,
                port_64,
                i64_ty.const_int(65535, false),
                "bind_port_gt_max",
            )
            .map_err(|e| format!("bind port gt error: {}", e))?;
        let is_bad_port = self
            .builder
            .build_or(bad_port, port_gt_max, "bind_bad_port")
            .map_err(|e| format!("bind port or error: {}", e))?;
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("bind: no current function".into()))?;
        let port_ok_bb = self.context.append_basic_block(function, "bind_port_ok_bb");
        let port_trap_bb = self
            .context
            .append_basic_block(function, "bind_port_trap_bb");
        self.builder
            .build_conditional_branch(is_bad_port, port_trap_bb, port_ok_bb)
            .map_err(|e| format!("bind cbr error: {}", e))?;
        self.builder.position_at_end(port_trap_bb);
        let abort_fn = self.get_or_declare_abort_fn();
        let msg = self
            .builder
            .build_global_string_ptr("bind: port must be in 0..=65535", "bind_port_msg")
            .map_err(|e| format!("bind global string error: {}", e))?;
        self.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(msg.as_pointer_value())],
            "bind_port_abort",
        )?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| format!("bind unreachable error: {}", e))?;
        self.builder.position_at_end(port_ok_bb);
        let func = self
            .module
            .get_function("mimi_bind")
            .ok_or("mimi_bind not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(fd),
                    BasicMetadataValueEnum::IntValue(port),
                ],
                "bind_call",
            )
            .map_err(|e| format!("bind error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_bind returned void")?)
    }

    pub(super) fn compile_listen(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "listen expects 2 arguments (fd, backlog)".to_string(),
            ));
        }
        let fd = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "listen: fd must be i32".to_string(),
                ))
            }
        };
        let backlog = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "listen: backlog must be i32".to_string(),
                ))
            }
        };
        let func = self
            .module
            .get_function("mimi_listen")
            .ok_or("mimi_listen not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(fd),
                    BasicMetadataValueEnum::IntValue(backlog),
                ],
                "listen_call",
            )
            .map_err(|e| format!("listen error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_listen returned void")?)
    }

    pub(super) fn compile_accept(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "accept expects 1 argument (fd)".to_string(),
            ));
        }
        let fd = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "accept: fd must be i32".to_string(),
                ))
            }
        };
        let func = self
            .module
            .get_function("mimi_accept")
            .ok_or("mimi_accept not declared")?;
        let result = self
            .builder
            .build_call(func, &[BasicMetadataValueEnum::IntValue(fd)], "accept_call")
            .map_err(|e| format!("accept error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_accept returned void")?)
    }

    pub(super) fn compile_send(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "send expects 2 arguments (fd, data)".to_string(),
            ));
        }
        let fd = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "send: fd must be i32".to_string(),
                ))
            }
        };
        // Use the Mimi string's explicit length when available so embedded
        // NUL bytes are sent intact (VM parity) rather than truncating at
        // the first NUL via strlen.
        let (data_ptr, data_len) = self.extract_raw_str_ptr_len(&args[1])?;
        let func = self
            .module
            .get_function("mimi_send")
            .ok_or("mimi_send not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(fd),
                    BasicMetadataValueEnum::PointerValue(data_ptr),
                    BasicMetadataValueEnum::IntValue(data_len),
                ],
                "send_call",
            )
            .map_err(|e| format!("send error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_send returned void")?)
    }

    pub(super) fn compile_recv(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "recv expects 2 arguments (fd, buf_size)".to_string(),
            ));
        }
        let fd = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "recv: fd must be i32".to_string(),
                ))
            }
        };
        let buf_size = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "recv: buf_size must be i32".to_string(),
                ))
            }
        };
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("recv: no enclosing function".to_string()))?;
        // VM parity (interp builtin_recv): buf_size <= 0 is a loud error
        // BEFORE any fd use — "recv: buf_size must be positive".
        let fd64 = if fd.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(fd, i64_ty, "recv_fd_sext")
                .map_err(|e| format!("sext error: {}", e))?
        } else {
            fd
        };
        let bs64 = if buf_size.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(buf_size, i64_ty, "recv_bs_sext")
                .map_err(|e| format!("sext error: {}", e))?
        } else {
            buf_size
        };
        let bs_bad = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLE,
                bs64,
                i64_ty.const_zero(),
                "recv_bs_bad",
            )
            .map_err(|e| format!("icmp error: {}", e))?;
        let bs_too_big = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGT,
                bs64,
                i64_ty.const_int(100_000_000, false),
                "recv_bs_too_big",
            )
            .map_err(|e| format!("icmp error: {}", e))?;
        let bs_size_ok_bb = self
            .context
            .append_basic_block(function, "recv_bs_size_ok_bb");
        let bs_ok_bb = self.context.append_basic_block(function, "recv_bs_ok_bb");
        let bs_trap_bb = self.context.append_basic_block(function, "recv_bs_trap_bb");
        let bs_trap_big_bb = self
            .context
            .append_basic_block(function, "recv_bs_trap_big_bb");
        self.builder
            .build_conditional_branch(bs_bad, bs_trap_bb, bs_size_ok_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(bs_trap_bb);
        self.emit_net_trap("recv: buf_size must be positive", "recv_bs")?;
        self.builder.position_at_end(bs_size_ok_bb);
        self.builder
            .build_conditional_branch(bs_too_big, bs_trap_big_bb, bs_ok_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(bs_trap_big_bb);
        self.emit_net_trap(
            "recv: buf_size exceeds 100 MB cap (refusing unbounded allocation)",
            "recv_bs_big",
        )?;
        self.builder.position_at_end(bs_ok_bb);

        // Allocate an i64 on stack to receive out_len
        let out_len_alloca = self
            .builder
            .build_alloca(self.context.i64_type(), "recv_out_len")
            .map_err(|e| format!("alloca error: {}", e))?;
        let func = self
            .module
            .get_function("mimi_recv")
            .ok_or("mimi_recv not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(fd64),
                    BasicMetadataValueEnum::IntValue(bs64),
                    BasicMetadataValueEnum::PointerValue(out_len_alloca),
                ],
                "recv_call",
            )
            .map_err(|e| format!("recv error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("mimi_recv returned void")?
            .into_pointer_value();
        // Audit wave2 (red line §1.4): mimi_recv returns NULL on error and
        // does NOT abort; the old code packed {NULL, 0} into a Mimi string
        // so ECONNRESET-style failures surfaced as Ok(dangling string).
        // The VM raises Err("recv() failed: …") instead — trap loud. NOTE:
        // the runtime also NULLs on n == 0 (peer EOF), which the VM maps to
        // Ok(""); EOF parity needs a runtime contract change (agent RT) and
        // is escalated in the deliverable.
        let is_null = self
            .builder
            .build_is_null(result, "recv_is_null")
            .map_err(|e| format!("is_null error: {}", e))?;
        let ok_bb = self.context.append_basic_block(function, "recv_ok_bb");
        let null_trap_bb = self
            .context
            .append_basic_block(function, "recv_null_trap_bb");
        self.builder
            .build_conditional_branch(is_null, null_trap_bb, ok_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(null_trap_bb);
        self.emit_net_trap_2i(
            "recv() failed: fd=%ld, buf_size=%ld (network error)",
            fd64,
            bs64,
            "recv_null",
        )?;
        self.builder.position_at_end(ok_bb);
        // NOTE: not registered — returned value owns the allocation
        // Build Mimi string struct {i8*, i64} value directly (not pointer to struct)
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let out_len = self
            .builder
            .build_load(
                BasicTypeEnum::IntType(self.context.i64_type()),
                out_len_alloca,
                "recv_len",
            )
            .map_err(|e| format!("load error: {}", e))?;
        let str_val = self
            .builder
            .build_insert_value(string_ty.get_undef(), result, 0, "str_ptr")
            .map_err(|e| format!("insert ptr error: {}", e))?;
        let str_val = self
            .builder
            .build_insert_value(str_val, out_len, 1, "str_len")
            .map_err(|e| format!("insert len error: {}", e))?;
        Ok(str_val.into_struct_value().into())
    }

    pub(super) fn compile_close_fd(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "close_fd expects 1 argument (fd)".to_string(),
            ));
        }
        let fd = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "close_fd: fd must be i32".to_string(),
                ))
            }
        };
        let func = self
            .module
            .get_function("mimi_close")
            .ok_or("mimi_close not declared")?;
        let result = self
            .builder
            .build_call(func, &[BasicMetadataValueEnum::IntValue(fd)], "close_call")
            .map_err(|e| format!("close error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_close returned void")?)
    }

    /// Phase D (0.39.76): 收 cap 的 net API——SystemToken 门禁在 args[1]
    /// （运行时忽略），url 在 args[0]。复用 http_get 核心。
    pub(super) fn compile_http_get_guarded(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "http_get_guarded expects 2 arguments (url, a SystemToken capability)".to_string(),
            ));
        }
        self.compile_http_get(&args[0..1])
    }

    pub(super) fn compile_http_get(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "http_get expects 1 argument (url)".to_string(),
            ));
        }
        let url_ptr = self.extract_raw_str_ptr(&args[0])?;
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("http_get: no enclosing function".to_string())
        })?;
        let func = self
            .module
            .get_function("mimi_http_get")
            .ok_or("mimi_http_get not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[BasicMetadataValueEnum::PointerValue(url_ptr)],
                "http_get_call",
            )
            .map_err(|e| format!("http_get error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("mimi_http_get returned void")?
            .into_pointer_value();
        // Audit wave2 (red line §1.4): mimi_http_get returns NULL on error
        // (resolve failure, refused, send/recv error, HTTPS unsupported).
        // The old code substituted an empty string — ECONNRESET & friends
        // surfaced as Ok("") instead of failing loud like the VM. NULL →
        // trap; a genuine empty response BODY still arrives as a non-NULL
        // allocated "" (runtime distinguishes body from failure).
        let is_null = self
            .builder
            .build_is_null(result, "http_is_null")
            .map_err(|e| format!("is_null error: {}", e))?;
        let ok_bb = self.context.append_basic_block(function, "http_get_ok_bb");
        let trap_bb = self
            .context
            .append_basic_block(function, "http_get_null_trap_bb");
        self.builder
            .build_conditional_branch(is_null, trap_bb, ok_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(trap_bb);
        self.emit_net_trap("http_get: request failed", "http_get_null")?;
        self.builder.position_at_end(ok_bb);
        // NOTE: not registered — returned value owns the allocation
        // Build Mimi string struct {i8*, i64}
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let str_alloca = self
            .builder
            .build_alloca(string_ty, "http_str")
            .map_err(|e| format!("alloca error: {}", e))?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| format!("gep error: {}", e))?;
        self.builder
            .build_store(ptr_gep, result)
            .map_err(|e| format!("store error: {}", e))?;
        let len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| format!("gep error: {}", e))?;
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let str_len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(result)],
                "http_strlen",
            )
            .map_err(|e| format!("strlen error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?;
        self.builder
            .build_store(len_gep, str_len)
            .map_err(|e| format!("store error: {}", e))?;
        self.build_load(
            BasicTypeEnum::StructType(string_ty),
            str_alloca,
            "http_get_str",
        )
        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))
    }

    pub(super) fn compile_http_post(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "http_post expects 2 arguments (url, body)".to_string(),
            ));
        }
        let url_ptr = self.extract_raw_str_ptr(&args[0])?;
        let body_ptr = self.extract_raw_str_ptr(&args[1])?;
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("http_post: no enclosing function".to_string())
        })?;
        let func = self
            .module
            .get_function("mimi_http_post")
            .ok_or("mimi_http_post not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(url_ptr),
                    BasicMetadataValueEnum::PointerValue(body_ptr),
                ],
                "http_post_call",
            )
            .map_err(|e| format!("http_post error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("mimi_http_post returned void")?
            .into_pointer_value();
        // Audit wave2 (red line §1.4): NULL means failure — trap loud like
        // the VM (old code substituted an empty string → silent Ok("")).
        let is_null = self
            .builder
            .build_is_null(result, "http_post_is_null")
            .map_err(|e| format!("is_null error: {}", e))?;
        let ok_bb = self.context.append_basic_block(function, "http_post_ok_bb");
        let trap_bb = self
            .context
            .append_basic_block(function, "http_post_null_trap_bb");
        self.builder
            .build_conditional_branch(is_null, trap_bb, ok_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(trap_bb);
        self.emit_net_trap("http_post: request failed", "http_post_null")?;
        self.builder.position_at_end(ok_bb);
        // NOTE: not registered — returned value owns the allocation
        // Build Mimi string struct {i8*, i64}
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let str_alloca = self
            .builder
            .build_alloca(string_ty, "http_str")
            .map_err(|e| format!("alloca error: {}", e))?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| format!("gep error: {}", e))?;
        self.builder
            .build_store(ptr_gep, result)
            .map_err(|e| format!("store error: {}", e))?;
        let len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| format!("gep error: {}", e))?;
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let str_len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(result)],
                "http_strlen",
            )
            .map_err(|e| format!("strlen error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?;
        self.builder
            .build_store(len_gep, str_len)
            .map_err(|e| format!("store error: {}", e))?;
        self.build_load(
            BasicTypeEnum::StructType(string_ty),
            str_alloca,
            "http_post_str",
        )
        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))
    }
}

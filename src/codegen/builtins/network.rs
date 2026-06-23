use super::CodeGenerator;
use super::super::call_try_basic_value;
use crate::error::{CompileError, MimiResult};
use super::super::CallSiteValueExt;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {

    pub(super) fn compile_socket(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 3 { return Err(CompileError::WrongArgCount("socket expects 3 arguments".to_string())); }
                let domain = match args[0] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("socket: domain must be i32".to_string())) };
                let type_ = match args[1] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("socket: type must be i32".to_string())) };
                let protocol = match args[2] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("socket: protocol must be i32".to_string())) };
                let func = self.module.get_function("mimi_socket")
                    .ok_or("mimi_socket not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(domain),
                    BasicMetadataValueEnum::IntValue(type_),
                    BasicMetadataValueEnum::IntValue(protocol),
                ], "socket_call")
                    .map_err(|e| format!("socket error: {}", e))?;
                Ok(call_try_basic_value(&result)
                    .ok_or("mimi_socket returned void")?)

    }

    pub(super) fn compile_connect(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 3 { return Err(CompileError::WrongArgCount("connect expects 3 arguments (fd, host, port)".to_string())); }
                let fd = match args[0] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("connect: fd must be i32".to_string())) };
                let host_ptr = self.extract_raw_str_ptr(&args[1])?;
                let port = match args[2] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("connect: port must be i32".to_string())) };
                let func = self.module.get_function("mimi_connect")
                    .ok_or("mimi_connect not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(fd),
                    BasicMetadataValueEnum::PointerValue(host_ptr),
                    BasicMetadataValueEnum::IntValue(port),
                ], "connect_call")
                    .map_err(|e| format!("connect error: {}", e))?;
                Ok(call_try_basic_value(&result)
                    .ok_or("mimi_connect returned void")?)

    }

    pub(super) fn compile_bind(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("bind expects 2 arguments (fd, port)".to_string())); }
                let fd = match args[0] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("bind: fd must be i32".to_string())) };
                let port = match args[1] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("bind: port must be i32".to_string())) };
                let func = self.module.get_function("mimi_bind")
                    .ok_or("mimi_bind not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(fd),
                    BasicMetadataValueEnum::IntValue(port),
                ], "bind_call")
                    .map_err(|e| format!("bind error: {}", e))?;
                Ok(call_try_basic_value(&result)
                    .ok_or("mimi_bind returned void")?)

    }

    pub(super) fn compile_listen(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("listen expects 2 arguments (fd, backlog)".to_string())); }
                let fd = match args[0] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("listen: fd must be i32".to_string())) };
                let backlog = match args[1] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("listen: backlog must be i32".to_string())) };
                let func = self.module.get_function("mimi_listen")
                    .ok_or("mimi_listen not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(fd),
                    BasicMetadataValueEnum::IntValue(backlog),
                ], "listen_call")
                    .map_err(|e| format!("listen error: {}", e))?;
                Ok(call_try_basic_value(&result)
                    .ok_or("mimi_listen returned void")?)

    }

    pub(super) fn compile_accept(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("accept expects 1 argument (fd)".to_string())); }
                let fd = match args[0] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("accept: fd must be i32".to_string())) };
                let func = self.module.get_function("mimi_accept")
                    .ok_or("mimi_accept not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(fd),
                ], "accept_call")
                    .map_err(|e| format!("accept error: {}", e))?;
                Ok(call_try_basic_value(&result)
                    .ok_or("mimi_accept returned void")?)

    }

    pub(super) fn compile_send(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("send expects 2 arguments (fd, data)".to_string())); }
                let fd = match args[0] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("send: fd must be i32".to_string())) };
                let data_ptr = self.extract_raw_str_ptr(&args[1])?;
                // Get string length via strlen
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let data_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(data_ptr),
                ], "send_strlen")
                    .map_err(|e| format!("strlen error: {}", e))?
                    .try_as_basic_value_opt()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let func = self.module.get_function("mimi_send")
                    .ok_or("mimi_send not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(fd),
                    BasicMetadataValueEnum::PointerValue(data_ptr),
                    BasicMetadataValueEnum::IntValue(data_len),
                ], "send_call")
                    .map_err(|e| format!("send error: {}", e))?;
                Ok(call_try_basic_value(&result)
                    .ok_or("mimi_send returned void")?)

    }

    pub(super) fn compile_recv(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("recv expects 2 arguments (fd, buf_size)".to_string())); }
                let fd = match args[0] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("recv: fd must be i32".to_string())) };
                let buf_size = match args[1] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("recv: buf_size must be i32".to_string())) };
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                // Allocate an i64 on stack to receive out_len
                let out_len_alloca = self.builder.build_alloca(self.context.i64_type(), "recv_out_len")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let func = self.module.get_function("mimi_recv")
                    .ok_or("mimi_recv not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(fd),
                    BasicMetadataValueEnum::IntValue(buf_size),
                    BasicMetadataValueEnum::PointerValue(out_len_alloca),
                ], "recv_call")
                    .map_err(|e| format!("recv error: {}", e))?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_recv returned void")?
                    .into_pointer_value();
                // NOTE: not registered — returned value owns the allocation
                // Build Mimi string struct {i8*, i64} value directly (not pointer to struct)
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let out_len = self.builder.build_load(
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    out_len_alloca, "recv_len"
                ).map_err(|e| format!("load error: {}", e))?;
                let str_val = self.builder.build_insert_value(
                    string_ty.get_undef(), result, 0, "str_ptr"
                ).map_err(|e| format!("insert ptr error: {}", e))?;
                let str_val = self.builder.build_insert_value(
                    str_val, out_len, 1, "str_len"
                ).map_err(|e| format!("insert len error: {}", e))?;
                Ok(str_val.into_struct_value().into())

    }

    pub(super) fn compile_close_fd(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("close_fd expects 1 argument (fd)".to_string())); }
                let fd = match args[0] { BasicMetadataValueEnum::IntValue(iv) => iv, _ => return Err(CompileError::TypeMismatch("close_fd: fd must be i32".to_string())) };
                let func = self.module.get_function("mimi_close")
                    .ok_or("mimi_close not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(fd),
                ], "close_call")
                    .map_err(|e| format!("close error: {}", e))?;
                Ok(call_try_basic_value(&result)
                    .ok_or("mimi_close returned void")?)

    }

    pub(super) fn compile_http_get(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("http_get expects 1 argument (url)".to_string())); }
                let url_ptr = self.extract_raw_str_ptr(&args[0])?;
                let func = self.module.get_function("mimi_http_get")
                    .ok_or("mimi_http_get not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::PointerValue(url_ptr),
                ], "http_get_call")
                    .map_err(|e| format!("http_get error: {}", e))?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_http_get returned void")?
                    .into_pointer_value();
                // NOTE: not registered — returned value owns the allocation
                // Build Mimi string struct {i8*, i64}
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "http_str")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let ptr_gep = self.gep().build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(ptr_gep, result)
                    .map_err(|e| format!("store error: {}", e))?;
                let len_gep = self.gep().build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let str_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(result),
                ], "http_strlen")
                    .map_err(|e| format!("strlen error: {}", e))?
                    .try_as_basic_value_opt()
                    .ok_or("strlen returned void")?;
                self.builder.build_store(len_gep, str_len)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(str_alloca.into())

    }

    pub(super) fn compile_http_post(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("http_post expects 2 arguments (url, body)".to_string())); }
                let url_ptr = self.extract_raw_str_ptr(&args[0])?;
                let body_ptr = self.extract_raw_str_ptr(&args[1])?;
                let func = self.module.get_function("mimi_http_post")
                    .ok_or("mimi_http_post not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::PointerValue(url_ptr),
                    BasicMetadataValueEnum::PointerValue(body_ptr),
                ], "http_post_call")
                    .map_err(|e| format!("http_post error: {}", e))?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_http_post returned void")?
                    .into_pointer_value();
                // NOTE: not registered — returned value owns the allocation
                // Build Mimi string struct {i8*, i64}
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "http_str")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let ptr_gep = self.gep().build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(ptr_gep, result)
                    .map_err(|e| format!("store error: {}", e))?;
                let len_gep = self.gep().build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let str_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(result),
                ], "http_strlen")
                    .map_err(|e| format!("strlen error: {}", e))?
                    .try_as_basic_value_opt()
                    .ok_or("strlen returned void")?;
                self.builder.build_store(len_gep, str_len)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(str_alloca.into())

    }

}

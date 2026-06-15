rule "每个阶段都必须是可编译、可测试的"
rule "Mimi 实现不直接修改 mimispec，只参考其设计"

module MimiCompiler:
    desc "Mimi v1.0 参考实现（Rust）"

    module Syntax:
        desc "词法、语法、AST"
        func lex:
            desc "输入源码，输出 TokenStream"
            ...
        func parse_mimi:
            desc "解析 .mimi 生产模式，输出 AST"
            ...
        func parse_mms:
            desc "v0.2 起支持 .mms 草图模式，输出 SketchAST"
            ...

    module Core:
        desc "语义分析核心"
        func resolve_names:
            desc "名称解析，输出 ResolvedAST"
            ...
        func type_check:
            desc "类型检查，输出 TypedAST"
            ...
        func borrow_check:
            desc "借用与所有权检查，输出 OwnershipGraph"
            ...

    module Runtime:
        desc "执行与并发运行时"
        module Scheduler:
            desc "actor 调度器"
            func spawn_actor:
                desc "创建 actor 实例，返回 ActorRef"
                ...
            func send_message:
                desc "向 actor 发消息，返回 Future"
                ...
        func interpret:
            desc "树遍历解释器，输出 Value"
            ...
        func run_parasteps:
            desc "v0.3 起支持结构化并发块"
            ...

    module Driver:
        desc "CLI 与构建入口"
        func main:
            desc "命令行入口"
            ...
        func check:
            desc "语法与语义检查，输出 Diagnostics"
            ...
        func run:
            desc "解释执行，返回 ExitCode"
            ...
        func build:
            desc "v0.4 起编译为可执行产物，返回 Artifact"
            ...

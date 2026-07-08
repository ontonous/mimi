# 安全策略 / Security Policy

## 报告安全漏洞 / Reporting a Vulnerability

如果你发现 Mimi 项目存在安全漏洞，**请不要公开提交 Issue**。请通过以下方式私下报告：

_If you find a security vulnerability in the Mimi project, **please do not file a public issue**. Instead, report it privately via:_

- **Email**: ontonous@gmail.com
- **PGP Key**: TBD（如启用）

### 报告内容 / What to Include

请提供以下信息以帮助我们快速响应：

_Please provide the following to help us respond quickly:_

1. 漏洞描述 / Description of the vulnerability
2. 影响范围 / Affected components and versions
3. 复现步骤 / Steps to reproduce
4. 建议的修复方案（可选） / Suggested fix (optional)

### 响应时间 / Response Timeline

| 阶段 | 时间 |
|---|---|
| 确认收到 | 48 小时内 |
| 初步评估 | 5 个工作日内 |
| 修复发布 | 取决于严重程度 |

---

## 安全相关配置 / Security-Related Config

### 已知的安全类问题

已知问题列表见 [CHANGELOG](CHANGELOG.md) 中的 `### Security` 章节。

最近的审计修复：

| 问题 | 版本 | 描述 |
|---|---|---|---|
| v0.28.26 多项修复 | v0.28.26 | Mutex 真正互斥、Channel 全局死锁修复、no_panic sigsetjmp UB→fork 隔离、heap slot dominance 修复 |
| Item 5 | v0.17 | GEP 安全抽象：消除 62 处 unsafe 指针算术 |
| Item 1/4/6/9 | v0.15 | C runtime → Rust 重写，消除线程竞态/JSON 深度/字符串边界/Map 零容量问题 |
| F-16 ~ F-20 | v0.12 | FFI crash 保护、回调死锁、信号恢复 |

### 运行时安全 / Runtime Safety

- **合约验证**：`--verify-contracts` 编译为运行时断言
- **边界检查**：列表索引操作有运行时边界断言（inbounds GEP + `mimi_runtime_abort`）
- **内存安全**：Rust 分配器全局管理，无手动 `malloc`/`free`

---

## 安全更新 / Security Updates

安全修复会标记在 CHANGELOG 的 `### Security` 章节，并在 commit 信息中标注 `Security` 标签。

---

## 致谢 / Acknowledgments

感谢所有负责任披露安全问题的贡献者。

_We thank all contributors who responsibly disclose security issues._

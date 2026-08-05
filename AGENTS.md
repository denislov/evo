## 语言选择

使用中文跟用户沟通，专业、常用的术语可以使用英文。任务计划文档也尽量使用中文，但专业词、代码相关内容使用英文。

## 工作原则
- 要有大局观，不要为了兼容性测试写冗余代码，可以做执行债务记录，推迟收敛，但在计划完整收敛时，所有债务记录需要处理掉。
- Do not preserve backward compatibility. Remove obsolete paths instead of adding compatibility layers, fallbacks, or migrations.
- Choose the simplest implementation that fully meets the current requirements. Avoid speculative abstractions, configuration, and indirection.
- Grow the system in layers. Start from the smallest version that works end to end, and add each new capability on top of a product that already works. Never trade a working product for unfinished complexity.
- Keep components modular and concerns clearly separated.
- Prefer established, well-maintained libraries when they reduce overall complexity or improve reliability. Do not reimplement common functionality without a clear reason.
- Lean on the dependencies already in the project before writing your own implementation or adding packages. Do not assume a library lacks a capability without checking its documentation and types.
- Make architectural decisions for the long term. Do not accept a stopgap that only works for now and is meant to be replaced later.
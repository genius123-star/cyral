# 前端结构

`main.tsx` 渲染 React 应用；`App.tsx` 按 Tauri 窗口标签选择桌宠、输入框或控制面板，并连接 Rust 命令和事件。`styles/global.css` 提供窗口共用样式。

| `features/` 子目录 | 职责 |
| --- | --- |
| `pet/` | `PetSurface` 渲染桌宠；`pet-manifest` 读取动画清单；`thinking-animation` 选择思考动作 |
| `chat/` | `ChatComposer` 管理输入；`ConversationHistory` 展示记录；`chat-request` 协调请求；`bubble-duration` 计算气泡显示时长 |
| `companion/` | `ActivityTimeline` 展示活动和关注主题；`ModeStatus` 展示模式；`presence-state` 处理陪伴状态 |
| `email/` | `EmailSettings` 管理账号配置；`EmailReviewCard` 展示待审核草稿并收集确认 |
| `settings/` | 模型、视觉、隐私和应用通用设置 |
| `organizer/` | `MemoryLibrary` 管理记忆；`TodoList` 管理待办和确认 |
| `panel/` | `ControlPanel` 组装各功能页签；`UsagePanel` 展示用量 |

前端行为测试集中在 [`tests/`](../tests/)，与对应组件或逻辑同名。会话持久化、权限判定和系统操作由 [Rust 后端](../src-tauri/README.md) 负责。

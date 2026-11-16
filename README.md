# Cyral（希莉尔）

**Cyral 是一个面向 Windows 的桌面 AI 伙伴。** 它以桌宠的形式待在桌面上：点一下就能继续对话，之前聊过的内容和确认过的待办可以留在本机。在你主动授权后，它也可以根据有限的桌面信息提供陪伴式提醒。

## 为什么做 Cyral

日常工作中，找 AI 问一句通常要切到另一个窗口，重新交代上下文；聊完之后，想记下的事情又要另开工具。Cyral 最初从一个个人桌宠原型出发，希望把这几个动作缩短成一条路径：**从桌面发起对话 → 延续上下文 → 把有用的信息留下，再回到手头的事。** 项目先面向单人 Windows 使用，重点是连续的对话体验和可管理的本地记忆；读屏、读文件、发邮件等操作由各自的授权或确认控制。

## 功能与实现

应用由三层组成：React 和 TypeScript 负责桌宠、输入框和控制面板；Tauri 连接窗口和 Rust 后端；Rust 处理模型请求、权限判断、系统集成和 SQLite 数据读写。SQLite 是随应用使用的本地数据库，不需要单独部署。

| 功能 | 使用体验 | 如何实现 |
| --- | --- | --- |
| 桌宠与流式对话 | 点击角色打开输入框，回复生成时逐步显示；右键进入控制面板 | Tauri 管理三个窗口，React 按动画清单播放 PNG 帧图。Rust 请求配置的模型服务，支持 OpenAI 风格的 Responses 或 Chat Completions 接口，并把回复片段送回窗口 |
| 对话记录与记忆 | 回看对话，查看、修改或删除提取的记忆 | Rust 将对话与记忆写入 SQLite；模型从对话中提取可复用信息，后续请求按需召回 |
| 待办与启动简报 | 从对话生成待办草稿，确认后创建；启动时查看当天概况 | Rust 定时检查到期提醒；启动简报使用固定模板，可汇总日期、天气、邮件和待办。配置缺失或查询失败时省略对应内容 |
| 陪伴观察 | 授权后可开启陪伴模式，随时停止 | Rust 在授权和模式检查通过后使用 Windows 接口采样；配置视觉模型后，观察功能可能将截图发送给该服务 |
| 拖入文件 | 文件拖进输入框先显示候选卡；明确要求后才读取受支持的文本 | Tauri 接收 Windows 拖拽事件，Rust 在当前会话保存临时候选。PDF、Office、图片和压缩包当前不提供内容解析 |
| 邮件助手 | 查看新邮件提醒，审核草稿和附件后手动确认发送 | Rust 使用 IMAP 收件、SMTP 发信；Gmail 走 OAuth 授权。发送前重新校验附件，未经确认不会发送 |

当前处于开发阶段。陪伴观察、启动简报、原生文件拖拽和各邮箱服务商仍需分别验证真实 Windows 使用场景；实现了功能不代表已完成端到端验收。已知范围见[产品路线](docs/PRODUCT_ROADMAP-v0.7.md)和[启动简报与拖拽说明](docs/PLAN-v0.7-startup-and-drop-files.md)。

Cyral 的历史记录和设置保存在本机；**本地存储不等于离线推理**。使用远程模型时，请求及相关上下文会发往所配置的服务；启用邮件或天气时也会连接对应服务。Windows 下，模型 API Key 和邮箱凭据通过系统凭据管理器保存。SQLite 数据库在首次启动时自动创建，默认位于 `%APPDATA%\app.cyral.desktop\cyral.db`。

## 公开仓库结构

下面列的是公开版本的构建和运行所需结构。本机工具链、私人资料、旧构建产物和密钥不属于公开仓库。

```text
Cyral/
├── README.md                       # 项目总入口
├── docs/                           # 文档索引、产品方案、上传清单
├── src/
│   ├── main.tsx / App.tsx          # React 入口、窗口分流、Rust 接口
│   ├── features/
│   │   ├── pet/                    # 桌宠渲染与动画
│   │   ├── chat/                   # 输入框、对话历史、请求与气泡
│   │   ├── companion/              # 模式状态与活动显示
│   │   ├── email/                  # 邮箱账号配置与发件审核
│   │   ├── settings/               # 模型、隐私、视觉及通用设置
│   │   ├── organizer/              # 记忆与待办
│   │   └── panel/                  # 控制面板与用量
│   └── styles/                     # 窗口共用样式
├── src-tauri/
│   ├── src/                        # Rust 入口、邮件、活动、存储和 Windows 集成
│   ├── prompts/                    # 运行时系统提示词
│   ├── capabilities/               # Tauri 窗口权限
│   ├── gen/schemas/                # Tauri 权限配置的结构定义
│   ├── icons/                      # Windows 打包图标
│   ├── Cargo.toml / Cargo.lock      # Rust 依赖配置与锁定版本
│   └── tauri.conf.json             # 窗口、构建和打包配置
├── public/assets/characters/cyral/  # 动画清单和运行时帧图
├── tests/                          # 前端行为测试
└── package.json / package-lock.json # 前端脚本、依赖配置与锁定版本
```

更细的入口见[前端结构](src/README.md)、[Rust 与 Tauri 结构](src-tauri/README.md)和[文档索引](docs/README.md)。完整公开文件范围见 [GitHub 上传清单](docs/GITHUB_RELEASE_CHECKLIST.md)。

## 下载与安装

Cyral 是 Windows 桌面应用，安装在自己的电脑上，不需要部署网页服务器。

### 使用 Windows 安装包

如果仓库的 **Releases** 页面已有 Windows 安装包，在发布条目的 **Assets** 中选择适合系统架构的 `*-setup.exe` 或 `.msi`，运行后从开始菜单打开 Cyral。如果 Releases 暂无安装包，按下面的步骤从源码构建。

### 从源码运行

1. 在 GitHub 仓库页面选择 **Code → Download ZIP** 并解压；也可以在 **Code → HTTPS** 复制仓库地址，用 Git 克隆。进入包含 `package.json` 的项目目录。
2. 准备 Node.js 22.12 或更新的 22.x、npm、Rust stable 的 Windows MSVC 工具链、Visual Studio Build Tools 的 **使用 C++ 的桌面开发** 工作负载，以及 Microsoft Edge WebView2 Runtime。见 [Tauri 的 Windows 环境要求](https://v2.tauri.app/start/prerequisites/#windows)和 [Vite 的 Node.js 要求](https://vite.dev/guide/)。使用 ZIP 时不需要安装 Git。
3. 在项目根目录打开 PowerShell：

   ```powershell
   npm ci
   npm run tauri dev
   ```

   `npm ci` 按锁文件安装前端依赖；`tauri dev` 会编译 Rust、启动本机 `127.0.0.1:1420` 的 Vite 服务并打开桌宠。首次运行可能需要下载 Rust 依赖。启动桌宠不需要预先配置 API Key；发送模型请求前需要完成连接配置。

### 构建安装包

在已安装依赖的项目根目录执行：

```powershell
npm run tauri build
```

命令会先构建前端，再编译 Rust 并生成 Windows 安装包。没有自定义 Cargo 输出目录时，安装包默认位于 `src-tauri/target/release/bundle/nsis/` 或 `src-tauri/target/release/bundle/msi/`。对应工具或系统组件有问题时，参考 [Tauri Windows 打包指南](https://v2.tauri.app/distribute/windows-installer/)。

## 第一次使用

1. **连接模型。** 右键点击桌宠，进入 **模型配置 → 模型连接**，按服务商要求填写接口地址、模型名称和 API Key，或填写自己的兼容本地模型地址。保存后，左键点击桌宠打开输入框，发一条消息确认回复正常。
2. **试用记忆和待办。** 在控制面板查看 **对话记录** 和 **模型配置 → 记忆库**。可以发送“明天下午三点提醒我整理资料”，检查生成的草稿、确认时间后再创建；已建提醒在 **待办提醒** 查看。
3. **按需使用高级功能。** 要开启陪伴观察，先在 **隐私设置** 授权，再在对话框输入 `/陪伴模式`；输入 `/停止陪伴` 或双击角色可停止。要读本机文本，先把支持的文件拖入输入框，再明确提出读取请求。要连接邮箱，进入 **邮件助手** 配置测试账号；Gmail 还需配置自己的桌面 OAuth 客户端。任何发信操作都应先核对草稿、收件人与附件。

## 开发检查与发布

```powershell
npm run typecheck
npm run lint
npm test
cargo test --manifest-path src-tauri/Cargo.toml
```

许可证和角色素材再分发授权以正式发布版本附带的许可文件为准。首次公开前，请按 [GitHub 上传清单](docs/GITHUB_RELEASE_CHECKLIST.md)核验授权、公开文件和安装包。

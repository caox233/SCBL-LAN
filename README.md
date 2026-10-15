# SCBL LAN

Windows 局域网启动器与游戏修复/MOD 工程。当前基于 1.0 R2 整理；这轮只调整工程、构建和打包组织，不调整玩法。

## 玩家

完整解压匹配的程序包，运行 `SCBL.LAN.exe`，选择 `Blacklist_DX11_game.exe`。不要混换不同包的 DLL；所有玩家与服务端使用同一完整包。仅在可信局域网或加密组网环境使用，不应直接暴露公网。

## 开发

首次按 [构建说明](docs/BUILD.md) 装好编译工具、准备合法持有的构建资源，然后双击根目录 **`build.cmd`**。

它会依次检查环境、运行回归、构建四个组件、打包并验证。成功时显示 ZIP 的绝对路径；失败会停在具体步骤并保留日志，不会拿旧包冒充本次成功。

只检查环境：`build.cmd --check`。命令行入口：`python scripts/build.py all`。

完整构建需要 Windows、Visual Studio C++ 工具、固定 Rust nightly、.NET 10 SDK、CMake、protoc 以及本地资源。首次获取依赖需要网络；本地资源不会自动下载。见 [构建说明](docs/BUILD.md)。

**目前仅下载源码还不足以生成完整包**：游戏加载器、音乐等 20 项输入未纳入 Git，需另行准备匹配资源。公开分发方案仍待授权审查，不能用一键脚本绕过它。

## 源码布局

- `src/launcher`：WPF 启动器。
- `src/native/client/hooks`：主 DLL；`gameplay`：MOD 玩法 DLL。
- `src/native/server`：联机后台、协议和 SQLite 存储。
- `src/native/shared`：共享协议、发布身份与完整包校验约定。
- `tests`：直接调用生产代码的少量核心回归；Rust 单元测试与对应模块放在一起。
- `scripts`：构建、测试与完整包校验入口。
- `artifacts`：本地产物，不提交 Git。

## 发布边界

当前只做本地整理和用户验收，尚未公开发布。游戏原始加载器、音乐、图片、默认存档模板及打包的第三方二进制是本地构建输入，已排除 Git；公开代码和公开程序包均须分别完成授权与隐私检查。现有许可证与第三方声明保留，不将游戏资源自动视为本项目许可证覆盖的内容。

`LICENSE` 保留上游 GPL-3.0 文本；来源与依赖见 `THIRD_PARTY_NOTICES.txt`。项目维护会话、AI 指令、历史诊断、账号与真实存档不进入源码仓库。

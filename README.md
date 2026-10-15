# SCBL LAN

Windows 局域网启动器与游戏修复/MOD 工程。

公开仓库：[caox233/SCBL-LAN](https://github.com/caox233/SCBL-LAN)。

## 玩家

完整解压匹配的程序包，运行 `SCBL.LAN.exe`，选择 `Blacklist_DX11_game.exe`。不要混换不同包的 DLL；所有玩家与服务端使用同一完整包。仅在可信局域网或加密组网环境使用，不应直接暴露公网。

## 开发

首次按 [构建说明](docs/BUILD.md) 装好编译工具，完整下载或克隆此仓库，然后双击根目录 **`build.cmd`**。所需的 20 项构建资源已随仓库提供，不需要另找资源包。

它会依次检查环境、运行回归、构建四个组件、打包并验证。成功时显示 ZIP 的绝对路径；失败会停在具体步骤并保留日志，不会拿旧包冒充本次成功。

只检查环境：`build.cmd --check`。命令行入口：`python scripts/build.py all`。

完整构建需要 Windows、Visual Studio C++ 工具、固定 Rust nightly、.NET 10 SDK、CMake 和 protoc。首次获取代码依赖需要网络。构建资源通过仓库内清单验证，不能删除二进制资源后仍称为完整源码包。见 [构建说明](docs/BUILD.md)。

## 源码布局

- `src/launcher`：WPF 启动器。
- `src/native/client/hooks`：主 DLL；`gameplay`：MOD 玩法 DLL。
- `src/native/server`：联机后台、协议和 SQLite 存储。
- `src/native/shared`：共享协议、发布身份与完整包校验约定。
- `tests`：直接调用生产代码的少量核心回归；Rust 单元测试与对应模块放在一起。
- `scripts`：构建、测试与完整包校验入口。
- `artifacts`：本地产物，不提交 Git。

## 发布边界

仓库提供源码、一键构建入口和清单中的 20 项构建资源。游戏原始加载器、音乐、图片、默认存档模板、打包的第三方二进制和 DirectX 头文件保留来源声明，各自原有权利和使用条件不因本仓库公开而改变。

`LICENSE` 保留上游 GPL-3.0 文本；来源与依赖见 `THIRD_PARTY_NOTICES.txt`。项目维护会话、AI 指令、历史诊断、账号数据库和登录信息不进入源码仓库。默认存档模板为用户明确选定的配置快照，不包含整个个人存档目录或历史备份。

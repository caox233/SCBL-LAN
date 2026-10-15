# 一键构建（Windows x64）

## 最短流程

1. 下载源码 ZIP 后完整解压，或 Git clone。建议使用短路径，例如 `C:\src\SCBL-LAN`。
2. 首次安装下面的编译工具，并准备合法持有、与清单匹配的资源。工具只装一次；首次依赖恢复需要联网。
3. 双击仓库根目录 **`build.cmd`**。无需管理员，也无需打开 Visual Studio。
4. 等待 `BUILD_SUCCESS`，按输出路径取得完整 ZIP。运行时文件在 `artifacts/program`；最新结果在 `artifacts/BUILD-RESULT.json`。

脚本依次执行：检查工具与资源 → 回归测试 → 主 DLL、玩法 DLL、服务端、启动器 → 完整包校验。不会启动游戏、安装到游戏目录、修改真实账号存档、自动提权或上传 GitHub。

## 首次工具准备

| 工具 | 要求与官方来源 |
| --- | --- |
| Python | 3.11+，安装后能运行 `py -3` 或 `python`；[官方下载](https://www.python.org/downloads/windows/) |
| Git | 安装时允许命令行使用；[Git for Windows](https://gitforwindows.org/) |
| Visual Studio Build Tools | C++ 桌面构建工具，包含 MSVC x86/x64、Windows SDK、CMake；[工作负载说明](https://learn.microsoft.com/en-us/visualstudio/install/workload-component-id-vs-build-tools?view=vs-2022) |
| .NET SDK | **10.0.302 功能带**，根目录 `global.json` 允许同功能带补丁；不是仅 Runtime。不要只装其他功能带；[安装说明](https://learn.microsoft.com/en-us/dotnet/core/install/windows) |
| Rust | 通过 [rustup](https://rust-lang.org/tools/install/) 安装 Windows x64 MSVC 工具链管理器。仓库指定 `nightly-2025-10-15` 和 `i686-pc-windows-msvc`；rustup 首次按 `rust-toolchain.toml` 获取 |
| protoc | 本项目验证版本为 **35.1**。从[官方编译器说明](https://protobuf.dev/installation/)进入对应发布版本，将 `bin` 加入 PATH，或设置 `PROTOC` 为 `protoc.exe` 的完整路径 |

装完工具后重新打开构建窗口。可以先运行 `build.cmd --check`；缺项会汇总说明，不会开始编译。构建过程中每 15 秒提示仍在工作；依赖下载失败会保留原始输出供排查。

## 资源准备：公开源码目前仍有这个前置条件

`scripts/local-assets.json` 精确列出 20 项输入及大小、SHA256，包括图标、背景、音乐、FusionFix、采集工具、原始加载器、默认存档模板和 DirectX 头文件。这些文件不提交 Git。

**单独 clone 或下载源码 ZIP，不能凭空得到这些资源。** 当前只支持合法持有资源的本地构建；公开资源包仍需授权和敏感信息审查，不承诺尚不存在的公开下载地址，也不会从非官方位置静默下载。

可选两种准备方式：

- 将匹配的 **`build-assets.zip` 放在源码根目录**，双击 `build.cmd` 时自动校验并导入。ZIP 内使用清单中的相对路径，例如 `src/launcher/src/logo.ico`，不要再套一层顶级目录。
- 使用已有资源目录或 ZIP：`build.cmd --asset-dir "C:\local-build-assets"`，或 `build.cmd --asset-dir "C:\local-build-assets.zip"`。

导入只处理清单内文件；完整校验通过后才写入，已有不匹配文件会报错而非覆盖，不会提取 ZIP 内的其他内容。资源目录与 ZIP 均被 Git 忽略。

## 重复构建与输出

- 同名结果已存在时，自动生成带构建时间的新目录和新 ZIP，不覆盖旧包，也不改发布身份。
- 不允许同一源码目录同时构建/测试，避免缓存互相覆盖。退出后系统释放锁，无需手动删锁文件。
- 每条子命令的 stdout、stderr、退出码保存在 `artifacts/logs`。失败返回非零退出码，不输出 `BUILD_SUCCESS`。
- 正式更改版本时修改 `release-identity.json`，四个组件一起重建；不得拼接其他版本 DLL 或关闭校验。
- 默认存档与玩法 DLL 的绑定写在 `artifacts/generated`，不修改源码模板。

## 命令行与可选配置

```powershell
.\build.cmd                     # 完整一键流程
.\build.cmd --check             # 只检查环境和资源
python scripts/build.py test     # 核心测试
python scripts/build.py build    # 单独编译打包
python scripts/build.py verify   # 校验最近一次成功产物
python scripts/build.py verify --package "C:\path\to\package"
```

自动化环境可设置 `SCBL_NO_PAUSE=1` 或 `CI=1`，关闭批处理结尾的按键等待。

一般开发者无需 `build.local.json`。非默认工具路径、共享编译缓存或已缓存依赖的离线编译，可参考 `scripts/build.local.example.json`；这个文件不提交 Git。不要把维护者机器路径复制为公共配置。`offline` 只控制 Cargo 离线，不能保证 .NET 首次恢复也能离线。

## 验证边界

核心回归和包校验不能代替多人实机验收。本轮只改构建流程，不改变游戏玩法。Git 中保留必要测试，历史采样、真实账号存档、AI 行为文件和私人维护记录不进入仓库。

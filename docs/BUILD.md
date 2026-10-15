# 构建与验证

## 工具

- Windows 10/11 x64。
- Visual Studio Build Tools：MSVC x86/x64、Windows SDK、CMake。
- .NET SDK 10.0.302（根目录 global.json 固定 SDK 功能带）。
- Rust `nightly-2025-10-15`、`i686-pc-windows-msvc` target（native/rust-toolchain.toml 固定）。
- protoc 35.1；通过 `PROTOC` 指定路径。
- Python 3.11+ 标准库；Git 用于获取 Cargo 锁定的 Git 依赖。

普通安装使用 PATH、CARGO_HOME、RUSTUP_HOME。可选的 `build.local.json` 仅保存本机工具路径，不提交 Git。格式见 `scripts/build.local.example.json`。不要将其他维护目录配置复制为公共配置；路径不是源码依赖。

## 本地资源

`scripts/local-assets.json` 列出本轮已知匹配的资源、长度和 SHA256。资源保存在原工程相对位置，但已被 .gitignore 排除。合法持有资源的开发者可准备相同相对目录的资源目录，然后运行：

```powershell
python scripts/build.py assets --asset-dir C:\local-build-assets
```

该命令只复制缺失且哈希匹配的资源，不联网、不覆盖不匹配文件。完整启动器缺少资源时会明确失败，不生成缺功能程序。本地资源不代表已获得公开再分发许可；公开发布前单独审查。

## 命令

1. `doctor`：检查本地资源及工具是否可运行。
2. `test`：运行端口规划、后台任务生命周期，以及原生配置/插件 ABI 的既有测试。不启动游戏、不触碰真实账号存档。
3. `build`：依次构建主 DLL、玩法 DLL、服务端，再生成与玩法 DLL 绑定的存档清单，发布启动器，生成完整包和校验目录。
4. `verify`：校验完整包全部清单；`--package <目录>` 可指定其他完整包。

输出在 `artifacts`；每条构建命令的原始 stdout、stderr 和退出码保存在 `artifacts/logs`。生成的默认存档绑定在 `artifacts/generated`，不会修改源码模板。

已有同名发布目录时构建拒绝覆盖。正式新版本先修改根目录的 `release-identity.json`；重试构建应先将失败产物保留到另一个明确目录。源码版本、四个程序组件和 catalog 必须匹配，不允许通过关闭校验来凑包。

## 验证范围

核心测试不等同于完整实机验收。其余历史回归和采样证据留在本地维护档案；需要重新纳入时，先整理成不依赖历史文件路径的测试，不把大量旧探针直接搬回来。

本轮结构整理验收重点：启动及完整包校验、设置与音乐、创建/加入房间、MOD 加载与现有存档保留。两端使用同一完整包，不要求重复调参或重置存档。

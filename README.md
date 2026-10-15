# SCBL LAN

《细胞分裂：黑名单》PC 版的局域网联机工具，内置服务端和游戏修复。可选官方玩法，也有一套围绕“间谍 vs 佣兵”装备与侦测规则调整的 MOD。

**[下载最新版本](https://github.com/caox233/SCBL-LAN/releases/latest)** · [反馈问题](https://github.com/caox233/SCBL-LAN/issues)

## 功能

- **开房联机**：启动器内创建房主，其他玩家填写房主 IP 加入，不用另外配置服务端。
- **鼠标修复**：支持高回报率鼠标输入，修正佣兵走路与奔跑灵敏度不一致的问题。
- **视野调整**：支持 1.0–1.3 倍 FOV，官方和 MOD 模式都能用。
- **存档分开**：官方与 MOD 使用各自的存档；MOD 前三套预设也能改装备、改名字并保存。

## MOD 改了什么

- 静音鞋蹲走可避开 ATS，分身阻挡裤可制造 RFD 假信号；间谍被侦测时会收到“已暴露”提示。
- 调整电磁镜、热成像和声呐镜的视距与显示效果，热成像可识别摄像头。
- 无人机退出操控后仍能侦测，也可重新接管；超载衣可让范围内灯光熄灭 10 秒。

目前保留各模式原生人数上限。目镜在部分场景仍可能卡顿，已知问题见[版本说明](docs/RELEASE-NOTES.md)。

## 怎么用

需要自备游戏，仅支持 DX11。联机玩家使用相同版本和游戏模式，并处于同一局域网或虚拟局域网。

1. 下载 Releases 中的 `SCBL-LAN-*-windows.zip`，完整解压。
2. 运行 `SCBL.LAN.exe`，选择游戏的 `Blacklist_DX11_game.exe`。
3. 房主选“创建房主”，其他人选“加入房主”并填写房主 IP，再启动游戏。

## 从源码编译

按[构建说明](docs/BUILD.md)装好工具后，双击 `build.cmd`。也可用 [GitHub Actions](https://github.com/caox233/SCBL-LAN/actions/workflows/release.yml) 在线编译。

[代码结构](docs/ARCHITECTURE.md) · [许可证](LICENSE) · [第三方声明](THIRD_PARTY_NOTICES.txt)

using System;
namespace SplinterCellCNLauncher.Models;

public sealed class LauncherSettings
{
    public string Username { get; set; } = "";
    [System.Text.Json.Serialization.JsonIgnore]
    public string Password { get; set; } = "";
    public string ServerAddress { get; set; } = "127.0.0.1";
    public string GameDirectory { get; set; } = "";
    public string GameExecutable { get; set; } = "Blacklist_DX11_game.exe";
    public string GameplayMode { get; set; } = "MOD";
    public double FovScale { get; set; } = 1.2;
    public bool RememberPassword { get; set; } = true;
    public int CredentialStorageVersion { get; set; }
    public string BindOverride { get; set; } = "";
    public string LastBindIp { get; set; } = "";

    // 首次打开启动器时，只自动提示一次“是否覆盖已有本地存档”。
    // 不管玩家选择取消还是覆盖，都会置为 true，避免以后每次打开都骚扰玩家。
    public bool SaveOverwritePromptHandled { get; set; }

    // UI language. zh-CN = 中文，en-US = English.
    public string Language { get; set; } = "zh-CN";

    // 背景音乐开关。启动器每次打开只播放一遍，不循环。
    public bool MusicEnabled { get; set; } = true;

    // 首次打开启动器时自动显示一次指引，完成/跳过后不再自动弹。
    public bool GuideCompleted { get; set; }
}

using SplinterCellCNLauncher.Models;
using System.Text.Json;

using System;
using System.IO;
namespace SplinterCellCNLauncher.Services;

public sealed class LauncherSettingsService
{
    public string SettingsPath { get; } = Path.Combine(LogService.AppDataDir, "launcher_settings.json");

    public LauncherSettings Load()
    {
        try
        {
            if (!File.Exists(SettingsPath))
                return new LauncherSettings();

            string json = File.ReadAllText(SettingsPath);
            var settings=JsonSerializer.Deserialize<LauncherSettings>(json) ?? new LauncherSettings();
            // Older builds always wrote false; it was not a user choice. No legacy password is read.
            if(settings.CredentialStorageVersion==0)settings.RememberPassword=true;
            return settings;
        }
        catch (Exception ex)
        {
            LogService.Error(ex);
            return new LauncherSettings();
        }
    }

    public void Save(LauncherSettings settings)
    {
        Directory.CreateDirectory(LogService.AppDataDir);

        var copy = new LauncherSettings
        {
            Username = settings.Username,
            ServerAddress = settings.ServerAddress,
            GameDirectory = settings.GameDirectory,
            GameExecutable = "Blacklist_DX11_game.exe",
            GameplayMode = GameplayRuntime.Normalize(settings.GameplayMode),
            FovScale = GameplayRuntime.NormalizeFov(settings.FovScale),
            RememberPassword = settings.RememberPassword,
            CredentialStorageVersion = 1,
            LastBindIp = settings.LastBindIp,
            BindOverride = settings.BindOverride,
            Password = "",
            SaveOverwritePromptHandled = settings.SaveOverwritePromptHandled,
            Language = string.IsNullOrWhiteSpace(settings.Language) ? "zh-CN" : settings.Language,
            MusicEnabled = settings.MusicEnabled,
            GuideCompleted = settings.GuideCompleted
        };

        string json = JsonSerializer.Serialize(copy, new JsonSerializerOptions
        {
            WriteIndented = true
        });

        ScblLan.LocalData.AtomicWrite(SettingsPath, json);
    }
}

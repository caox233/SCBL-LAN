using Microsoft.Win32;
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.RegularExpressions;

namespace SplinterCellCNLauncher.Services;

public sealed class GameLocatorService
{
    private static readonly string[] RequiredExecutables =
    {
        "Blacklist_DX11_game.exe"
    };

    public void ValidateGameDirectory(string gameDir, string gameExecutable)
    {
        if (!string.Equals(gameExecutable, "Blacklist_DX11_game.exe", StringComparison.OrdinalIgnoreCase))
            throw new ArgumentException("SCBL LAN 1.0 仅支持 DX11，不支持 DX9 或其他程序。");

        if (string.IsNullOrWhiteSpace(gameDir))
            throw new Exception("游戏目录不能为空。");

        if (!Directory.Exists(gameDir))
            throw new Exception($"游戏目录不存在：{gameDir}");

        string gameExePath = Path.Combine(gameDir, gameExecutable);
        if (!File.Exists(gameExePath))
            throw new Exception($"未找到游戏主程序：{gameExePath}");
    }

    public bool IsValidGameDirectory(string? gameDir)
    {
        if (string.IsNullOrWhiteSpace(gameDir) || !Directory.Exists(gameDir))
            return false;

        bool hasGameExe = RequiredExecutables.Any(exe => File.Exists(Path.Combine(gameDir, exe)));
        return hasGameExe;
    }

    public string? TryAutoFindGameDirectory()
    {
        foreach (string candidate in EnumerateCandidateDirectories().Distinct(StringComparer.OrdinalIgnoreCase))
        {
            if (IsValidGameDirectory(candidate))
                return candidate;
        }

        return null;
    }

    private IEnumerable<string> EnumerateCandidateDirectories()
    {
        foreach (string path in EnumerateRegistryInstallDirs())
        {
            foreach (string candidate in ExpandPossibleSystemDirs(path))
                yield return candidate;
        }

        foreach (string path in EnumerateSteamLibraryDirs())
        {
            string common = Path.Combine(path, "steamapps", "common", "Tom Clancy's Splinter Cell Blacklist");
            foreach (string candidate in ExpandPossibleSystemDirs(common))
                yield return candidate;
        }

        string[] commonRoots =
        {
            @"C:\Program Files (x86)\Steam\steamapps\common\Tom Clancy's Splinter Cell Blacklist",
            @"C:\Program Files\Steam\steamapps\common\Tom Clancy's Splinter Cell Blacklist",
            @"D:\Steam\steamapps\common\Tom Clancy's Splinter Cell Blacklist",
            @"D:\steam\steamapps\common\Tom Clancy's Splinter Cell Blacklist"
        };

        foreach (string root in commonRoots)
        {
            foreach (string candidate in ExpandPossibleSystemDirs(root))
                yield return candidate;
        }
    }

    private static IEnumerable<string> ExpandPossibleSystemDirs(string installDir)
    {
        if (string.IsNullOrWhiteSpace(installDir))
            yield break;

        string normalized = installDir.Trim().Trim('"');

        yield return normalized;
        yield return Path.Combine(normalized, "SYSTEM");
        yield return Path.Combine(normalized, "src", "SYSTEM");

        if (normalized.EndsWith(Path.Combine("src", "SYSTEM"), StringComparison.OrdinalIgnoreCase))
            yield return normalized;
    }

    private static IEnumerable<string> EnumerateRegistryInstallDirs()
    {
        string[] keyPaths =
        {
            @"SOFTWARE\Ubisoft\Splinter Cell Blacklist",
            @"SOFTWARE\WOW6432Node\Ubisoft\Splinter Cell Blacklist"
        };

        RegistryHive[] hives =
        {
            RegistryHive.LocalMachine,
            RegistryHive.CurrentUser
        };

        foreach (RegistryHive hive in hives)
        {
            foreach (RegistryView view in new[] { RegistryView.Registry64, RegistryView.Registry32 })
            {
                using RegistryKey? root = TryOpenRegistryBaseKey(hive, view);
                if (root == null)
                    continue;

                foreach (string keyPath in keyPaths)
                {
                    string? installDir = TryReadRegistryInstallDir(root, keyPath);
                    if (!string.IsNullOrWhiteSpace(installDir))
                        yield return installDir;
                }
            }
        }
    }

    private static RegistryKey? TryOpenRegistryBaseKey(RegistryHive hive, RegistryView view)
    {
        try
        {
            return RegistryKey.OpenBaseKey(hive, view);
        }
        catch (Exception ex) when (ex is UnauthorizedAccessException or System.Security.SecurityException or IOException)
        {
            return null;
        }
    }

    private static string? TryReadRegistryInstallDir(RegistryKey root, string keyPath)
    {
        try
        {
            using RegistryKey? key = root.OpenSubKey(keyPath);
            return key?.GetValue("installdir") as string
                ?? key?.GetValue("InstallDir") as string
                ?? key?.GetValue("InstallPath") as string;
        }
        catch (Exception ex) when (ex is UnauthorizedAccessException or System.Security.SecurityException or IOException)
        {
            return null;
        }
    }

    private static IEnumerable<string> EnumerateSteamLibraryDirs()
    {
        string[] possibleSteamRoots =
        {
            @"C:\Program Files (x86)\Steam",
            @"C:\Program Files\Steam",
            @"D:\Steam",
            @"D:\steam"
        };

        foreach (string steamRoot in possibleSteamRoots)
        {
            if (Directory.Exists(steamRoot))
                yield return steamRoot;

            string vdf = Path.Combine(steamRoot, "steamapps", "libraryfolders.vdf");
            if (!File.Exists(vdf))
                continue;

            string text;
            try
            {
                text = File.ReadAllText(vdf);
            }
            catch
            {
                continue;
            }

            foreach (Match match in Regex.Matches(text, "\\\"path\\\"\\s+\\\"(?<path>[^\\\"]+)\\\""))
            {
                string path = match.Groups["path"].Value.Replace("\\\\", "\\");
                if (!string.IsNullOrWhiteSpace(path) && Directory.Exists(path))
                    yield return path;
            }
        }
    }
}

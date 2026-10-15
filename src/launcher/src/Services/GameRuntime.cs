using System.Diagnostics;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace ScblLan;

public sealed record InstallReceipt(string Target, string OriginalHash, string InstalledHash, bool CreatedOriginalForwarder, string? PreviousInstalledHash = null, string? BackupRelativePath = null, string? ForwarderHash = null, bool OriginalAbsent = false);

public static class GameRuntime
{
    public static readonly string[] ExecutableNames = ["Blacklist_DX11_game.exe"];
    // Legacy DX9 processes must still block DLL replacement, although they cannot receive launch credentials.
    private static readonly string[] ProtectedProcessNames = ["Blacklist_game.exe", "Blacklist_DX11_game.exe"];
    public static string ValidateExecutable(string path)
    {
        path = Path.GetFullPath(path);
        if (!File.Exists(path) || !ExecutableNames.Contains(Path.GetFileName(path), StringComparer.OrdinalIgnoreCase))
            throw new IOException("SCBL LAN 1.0 仅支持 DX11，请选择 Blacklist_DX11_game.exe；DX9 不再受支持。");
        using (var input = new BinaryReader(File.OpenRead(path)))
        {
            if (input.BaseStream.Length < 64 || input.ReadUInt16() != 0x5A4D) throw new IOException("所选文件不是有效 Windows 游戏程序。");
            input.BaseStream.Position = 0x3c; int pe = input.ReadInt32();
            if (pe < 64 || pe > input.BaseStream.Length - 6) throw new IOException("游戏 PE 头无效。");
            input.BaseStream.Position = pe;
            if (input.ReadUInt32() != 0x00004550 || input.ReadUInt16() != 0x014c) throw new IOException("此版本要求 x86 游戏程序。");
        }
        return path;
    }
    public static bool AnyGameRunning() => ProtectedProcessNames.Any(n =>
    {
        var processes = Process.GetProcessesByName(Path.GetFileNameWithoutExtension(n));
        try { return processes.Length != 0; } finally { foreach (var p in processes) p.Dispose(); }
    });
    public static string Hash(string path) { using var f = File.OpenRead(path); return Convert.ToHexString(SHA256.HashData(f)); }
    private static bool IsOwnedHash(string actual, InstallReceipt receipt)
        => actual == receipt.OriginalHash || actual == receipt.InstalledHash
        || receipt.PreviousInstalledHash is { Length: 64 } previous && actual == previous;
    public static void CopyVerified(string source, string target, string expected)
    {
        string tmp = target + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            File.Copy(source, tmp, false);
            if (Hash(tmp) != expected) throw new IOException("写入后的文件校验失败。");
            File.Move(tmp, target, true);
            if (Hash(target) != expected) throw new IOException("部署后的文件校验失败。");
        }
        finally { if (File.Exists(tmp)) File.Delete(tmp); }
    }
    public static void Deploy(string executable, string runtimeRoot)
        => DeployCore(executable, runtimeRoot, AnyGameRunning);

    internal static void DeployCore(string executable, string runtimeRoot, Func<bool> isGameRunning)
    {
        executable = ValidateExecutable(executable);
        if (isGameRunning()) throw new IOException("游戏正在运行，禁止覆盖组件。");
        string dir = Path.GetDirectoryName(executable)!;
        using var mutex = DeploymentMutex(dir);
        bool owned = false;
        try
        {
            try { owned = mutex.WaitOne(0); } catch (AbandonedMutexException) { owned = true; }
            if (!owned) throw new IOException("正在部署此游戏的组件，请稍后重试。");
            string source = Path.Combine(runtimeRoot, "uplay_r1_loader.dll");
            string hashFile = source + ".sha256";
            if (!File.Exists(source) || !File.Exists(hashFile)) throw new IOException("开发包缺少已构建的 LAN Hooks 及校验文件。");
            string expected = File.ReadAllText(hashFile).Trim().Split(' ', StringSplitOptions.RemoveEmptyEntries)[0].ToUpperInvariant();
            if (expected.Length != 64 || expected != Hash(source)) throw new IOException("LAN Hooks 包完整性校验失败。");
            string target = Path.Combine(dir, "uplay_r1_loader.dll");
            string marker = Path.Combine(dir, "scbl-lan.install.json");
            string forwarder = Path.Combine(dir, "uplay_r1_loader.orig.dll");
            foreach (string path in new[] { target, marker, forwarder }) CheckPlainPath(path);
            if (Directory.Exists(target) || Directory.Exists(forwarder)) throw new IOException("DLL 路径被同名目录占用，尚未覆盖文件。");
            if (File.Exists(Path.Combine(dir, "scbl_gameplay.dll"))) throw new IOException("该游戏目录含外部 MOD 组件。请使用独立游戏副本或先处理外部 scbl_gameplay.dll；本版不会覆盖或删除它。");
            string? before = File.Exists(target) ? Hash(target) : null;
            InstallReceipt? receipt = File.Exists(marker) ? JsonSerializer.Deserialize<InstallReceipt>(File.ReadAllText(marker)) ?? throw new IOException("安装记录无效。") : null;
            // Do not mistake a previous launcher's target for an authentic Ubisoft original.
            // It is a restore preimage; the runtime forwarding DLL has an independent identity.
            bool reuse = receipt != null && (before == null || IsOwnedHash(before, receipt));
            if (receipt?.OriginalAbsent == true && (receipt.OriginalHash != "" || receipt.BackupRelativePath != null))
                throw new IOException("LAN 安装记录中的空原文件状态无效。");
            string? backup = reuse && !receipt!.OriginalAbsent ? ReceiptBackup(dir, receipt) : null;
            if (backup != null && (!File.Exists(backup) || Hash(backup) != receipt!.OriginalHash))
                throw new IOException("LAN 接管前备份校验失败，保留现有文件并停止部署。");
            bool created = !File.Exists(forwarder);
            if (created)
            {
                string legacy = Path.Combine(dir, "uplay_r1_loader.scbl-lan.original.dll");
                var candidates = new[] { backup, legacy, target }.Where(p => p != null).Cast<string>();
                string? origin = candidates.FirstOrDefault(p => { CheckPlainPath(p); return File.Exists(p) && LoaderCompatibility.CanForward(p); });
                if ((File.Exists(target) ? Hash(target) : null) != before || isGameRunning())
                    throw new IOException("补齐组件前游戏或文件状态发生变化，请重试。");
                if (origin == null) BundledOriginalLoader.Create(forwarder);
                else
                {
                    // Existing local/receipt original takes priority over the bundled fallback.
                    string temp = forwarder + "." + Guid.NewGuid().ToString("N") + ".tmp";
                    try
                    {
                        string originalHash = Hash(origin); File.Copy(origin, temp, false);
                        if (Hash(temp) != originalHash || !LoaderCompatibility.CanForward(temp)) throw new IOException("原 DLL 备份校验失败。");
                        File.Move(temp, forwarder, false);
                    }
                    finally { if (File.Exists(temp)) File.Delete(temp); }
                }
            }
            else if (!LoaderCompatibility.CanForward(forwarder))
                RepairOriginalForwarder(forwarder, () => isGameRunning() || (File.Exists(target) ? Hash(target) : null) != before);
            string forwardHash = Hash(forwarder);
            if (!reuse)
            {
                if (before == null)
                    receipt = new InstallReceipt(target, "", expected, created, null, null, forwardHash, OriginalAbsent: true);
                else
                {
                string relative = Path.Combine(".scbl-backups", "hooks", before + ".dll");
                backup = Path.Combine(dir, relative); CheckPlainPath(backup);
                Directory.CreateDirectory(Path.GetDirectoryName(backup)!);
                if (!File.Exists(backup)) CopyVerified(target, backup, before);
                if (Hash(backup) != before) throw new IOException("已有同名接管备份内容不一致，拒绝覆盖备份。");
                receipt = new InstallReceipt(target, before, expected, created, before, relative, forwardHash);
                }
            }
            if ((File.Exists(target) ? Hash(target) : null) != before || Hash(forwarder) != forwardHash || isGameRunning())
                throw new IOException("部署前组件或游戏运行状态发生变化，尚未替换，请重试。");
            var pending = receipt! with { InstalledHash = expected, PreviousInstalledHash = before, ForwarderHash = forwardHash, CreatedOriginalForwarder = receipt!.CreatedOriginalForwarder || created };
            // Journal before atomic replacement; an interrupted or locked write remains resumable/restorable.
            LocalData.AtomicWrite(marker, JsonSerializer.Serialize(pending));
            if (before != expected) CopyVerified(source, target, expected);
            LocalData.AtomicWrite(marker, JsonSerializer.Serialize(pending with { PreviousInstalledHash = null }));
        }
        finally { if (owned) mutex.ReleaseMutex(); }
    }
    private static void RepairOriginalForwarder(string forwarder, Func<bool> stateChanged)
    {
        // A bad forwarding DLL is repairable, not a launch-time ownership/signature error.
        // File.Replace atomically saves the exact replaced bytes and installs our verified original.
        string before = Hash(forwarder);
        string backupDirectory = Path.Combine(Path.GetDirectoryName(forwarder)!, ".scbl-backups", "orig");
        string identity = Guid.NewGuid().ToString("N");
        string backup = Path.Combine(backupDirectory, before + "-" + identity + ".dll");
        string temp = forwarder + "." + identity + ".repair.tmp";
        foreach (string path in new[] { forwarder, backup, temp }) CheckPlainPath(path);
        try
        {
            BundledOriginalLoader.Create(temp);
            Directory.CreateDirectory(backupDirectory);
            foreach (string path in new[] { forwarder, backup, temp }) CheckPlainPath(path);
            if (stateChanged() || !File.Exists(forwarder) || Hash(forwarder) != before)
                throw new IOException("修复原版 DLL 前游戏或文件状态发生变化，请重试。");
            if (File.Exists(backup) || Directory.Exists(backup)) throw new IOException("原版 DLL 备份路径冲突，尚未覆盖。");
            // Do not clear read-only attributes or bypass sharing/permission failures.
            File.Replace(temp, forwarder, backup, false);
            if (Hash(backup) != before || Hash(forwarder) != BundledOriginalLoader.Sha256)
                throw new IOException("原版 DLL 修复后的校验失败；替换前文件已保存在 .scbl-backups/orig。");
        }
        finally { if (File.Exists(temp)) File.Delete(temp); }
    }
    private static Mutex DeploymentMutex(string dir) => new(false, "Local\\SCBL_LAN_Hooks_" + Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(Path.GetFullPath(dir).ToUpperInvariant()))));
    private static void CheckPlainPath(string path)
    {
        for (string? p = Path.GetFullPath(path); p != null; p = Path.GetDirectoryName(p))
            if ((File.Exists(p) || Directory.Exists(p)) && (File.GetAttributes(p) & FileAttributes.ReparsePoint) != 0)
                throw new IOException("组件路径含符号链接或目录联接，拒绝写入。");
    }
    private static string ReceiptBackup(string dir, InstallReceipt receipt)
    {
        string relative = receipt.BackupRelativePath ?? "uplay_r1_loader.scbl-lan.original.dll";
        if (receipt.OriginalHash.Length != 64 || !receipt.OriginalHash.All(Uri.IsHexDigit) ||
            (receipt.BackupRelativePath != null && !string.Equals(relative.Replace('\\', '/'), ".scbl-backups/hooks/" + receipt.OriginalHash + ".dll", StringComparison.OrdinalIgnoreCase)))
            throw new IOException("LAN 安装记录中的备份路径无效。");
        string result = Path.Combine(dir, relative); CheckPlainPath(result); return result;
    }
    public static void Restore(string executable) => RestoreCore(executable, AnyGameRunning);
    internal static void RestoreCore(string executable, Func<bool> isGameRunning)
    {
        executable = ValidateExecutable(executable);
        if (isGameRunning()) throw new IOException("请先退出游戏。");
        string dir = Path.GetDirectoryName(executable)!;
        using var mutex = DeploymentMutex(dir); bool owned = false;
        try
        {
            try { owned = mutex.WaitOne(0); } catch (AbandonedMutexException) { owned = true; }
            if (!owned) throw new IOException("正在部署此游戏的组件，请稍后重试。");
            string marker = Path.Combine(dir, "scbl-lan.install.json"); CheckPlainPath(marker);
            var receipt = JsonSerializer.Deserialize<InstallReceipt>(File.ReadAllText(marker)) ?? throw new IOException("没有有效 LAN 安装记录。");
            string target = Path.Combine(dir, "uplay_r1_loader.dll"); CheckPlainPath(target);
            if (Directory.Exists(target)) throw new IOException("DLL 路径被同名目录占用。");
            if (receipt.OriginalAbsent)
            {
                if (receipt.OriginalHash != "" || receipt.BackupRelativePath != null) throw new IOException("LAN 安装记录中的空原文件状态无效。");
                if (File.Exists(target))
                {
                    if (!IsOwnedHash(Hash(target), receipt) || isGameRunning()) throw new IOException("DLL 或游戏状态发生变化，拒绝删除。");
                    File.Delete(target);
                }
                // The preimage was absent; keep the independently repaired orig and journal.
                return;
            }
            string backup = ReceiptBackup(dir, receipt);
            if (File.Exists(target) && !IsOwnedHash(Hash(target), receipt)) throw new IOException("DLL 已被其他程序修改，拒绝还原覆盖。");
            if (Hash(backup) != receipt.OriginalHash) throw new IOException("接管前备份校验失败。");
            if (isGameRunning()) throw new IOException("请先退出游戏。");
            CopyVerified(backup, target, receipt.OriginalHash);
            // Keep all backups, the independent forwarder and journal. Never alter saves.
        }
        finally { if (owned) mutex.ReleaseMutex(); }
    }
    public static string Config(LanConnection connection, string username, string password, string accountId,string gameplayMode="Official",double fovScale=1.2,SplinterCellCNLauncher.Services.ReleasePackage? release=null)
    {
        static string Q(string s)
        {
            var result = new StringBuilder("\"");
            foreach (char ch in s)
            {
                if (ch is '\\' or '"') result.Append('\\').Append(ch);
                else if (ch < 0x20 || ch == 0x7f) result.Append("\\u").Append(((int)ch).ToString("X4"));
                else result.Append(ch);
            }
            return result.Append('"').ToString();
        }
        if (new[] { username, password, accountId }.Any(s => s.Contains('\0'))) throw new ArgumentException("身份字段含无效字符。");
        string releaseFrame=release is null?"":$"[Release]\nSchema = 1\nPackageRoot = {Q(release.PackageRoot)}\nReleaseId = {Q(release.ReleaseId)}\nCompatId = {Q(release.CompatId)}\nManifestSha256 = {Q(release.ManifestSha256)}\n";
        return $"ConfigServer = {Q(connection.ServerIp.ToString())}\nApiServer = {Q($"http://{connection.ServerIp}:{connection.Port}/")}\nGameplayMode = {Q(SplinterCellCNLauncher.Services.GameplayRuntime.Normalize(gameplayMode))}\nEnableOverlay = true\nAutoJoinInvite = false\n[User]\nUsername = {Q(username)}\nPassword = {Q(password)}\nAccountId = {Q(accountId)}\n[Networking]\nIpAddress = {Q(connection.LocalIp.ToString())}\n[Logging]\nLevel = \"INFO\"\n[Features]\nFovFactor = {SplinterCellCNLauncher.Services.GameplayRuntime.NormalizeFov(fovScale).ToString("0.00",System.Globalization.CultureInfo.InvariantCulture)}\n{releaseFrame}";
    }
}

public sealed class LaunchPipe : IAsyncDisposable
{
    public string Name { get; } = "scbl-lan-" + Guid.NewGuid().ToString("N");
    private readonly NamedPipeServerStream pipe;
    private readonly byte[] config;
    internal SplinterCellCNLauncher.Services.RequestedConfigurationProjection RequestedConfigurationProjection { get; }
    private readonly string expectedExecutable;
    private readonly DateTime started = DateTime.UtcNow;
    private int disposed;
    public LaunchPipe(string config, string executable)
    {
        this.config = Encoding.UTF8.GetBytes(config); expectedExecutable = Path.GetFullPath(executable);
        RequestedConfigurationProjection = SplinterCellCNLauncher.Services.RequestedConfigurationSnapshot.Prepare(this.config);
        pipe = new(Name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
    }
    public event Action<Process>? PeerValidated;
    public async Task DeliverAsync(CancellationToken ct)
    {
        ObjectDisposedException.ThrowIf(disposed != 0,this);
        await pipe.WaitForConnectionAsync(ct);
        try {
        using var peerTimeout=CancellationTokenSource.CreateLinkedTokenSource(ct);
        peerTimeout.CancelAfter(TimeSpan.FromSeconds(10));
        ct=peerTimeout.Token;
        if (!GetNamedPipeClientProcessId(pipe.SafePipeHandle, out uint pid)) throw new IOException("无法确认游戏进程身份。");
        using var client = Process.GetProcessById(checked((int)pid));
        string actual = client.MainModule?.FileName ?? "";
        string expectedDir = Path.GetDirectoryName(expectedExecutable)!;
        if (!string.Equals(Path.GetDirectoryName(actual), expectedDir, StringComparison.OrdinalIgnoreCase)
            || !GameRuntime.ExecutableNames.Contains(Path.GetFileName(actual), StringComparer.OrdinalIgnoreCase)
            || client.StartTime.ToUniversalTime() < started.AddSeconds(-1))
            throw new IOException("拒绝向非本次游戏进程交付认证信息。");
        // Diagnostic observers cannot block or alter credential delivery.
        if(PeerValidated is { } observers) foreach(Action<Process> observer in observers.GetInvocationList())
            try { observer(client); } catch { }
        await pipe.WriteAsync(BitConverter.GetBytes(config.Length), ct);
        await pipe.WriteAsync(config, ct);
        await pipe.FlushAsync(ct);
        byte[] acknowledgement = new byte[1];
        await pipe.ReadExactlyAsync(acknowledgement, ct);
        if (acknowledgement[0] != 1) throw new IOException("游戏未确认收到配置。");
        } finally {if(pipe.IsConnected) pipe.Disconnect();}
        // Keep only the in-memory frame for a later retail handoff process.
        // DisposeAsync zeroes it when the launch session ends.
    }
    public async ValueTask DisposeAsync() { if(Interlocked.Exchange(ref disposed,1)!=0) return;CryptographicOperations.ZeroMemory(config); await pipe.DisposeAsync(); }
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetNamedPipeClientProcessId(SafePipeHandle pipe, out uint clientProcessId);
}

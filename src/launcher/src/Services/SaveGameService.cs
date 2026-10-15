using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using ScblLan;

namespace SplinterCellCNLauncher.Services;

// Explicit user action only. Normal launch never reads/seeds/replaces player saves.
public sealed class SaveGameService
{
    private readonly string backups;
    private readonly Func<bool> running;
    internal Func<string, Dictionary<string, byte[]>>? TemplateOverride { get; set; }
    internal Action<int>? BeforeWrite { get; set; }
    public string SaveDirectory { get; }
    public SaveGameService(string? saveDirectory = null, string? backupDirectory = null, Func<bool>? gameRunning = null)
    {
        SaveDirectory = Path.GetFullPath(saveDirectory ?? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "5th-Echelon", "Saves"));
        backups = Path.GetFullPath(backupDirectory ?? Path.Combine(LogService.AppDataDir, "save-backups"));
        running = gameRunning ?? GameRuntime.AnyGameRunning;
    }
    public string DirectoryFor(string mode) => mode switch
    {
        "Official" => SaveDirectory,
        "MOD" => Path.Combine(SaveDirectory, "MOD"),
        _ => throw new ArgumentException("未知存档模式。")
    };
    private static string[] Names(string mode) => mode switch
    {
        "Official" => ["00000001.meta", "00000001.sav", "00000002.meta", "00000002.sav"],
        "MOD" => ["00000001.meta", "00000001.sav", "00000002.meta", "00000002.sav", "00000001.scbl-loadouts"],
        _ => throw new ArgumentException("未知存档模式。")
    };
    private static string Hash(byte[] bytes) => Convert.ToHexString(SHA256.HashData(bytes));
    private static byte[] Resource(string name)
    {
        using var s = typeof(SaveGameService).Assembly.GetManifestResourceStream("SCBL.LAN.DefaultSaves." + name)
            ?? throw new IOException("内置存档资源缺失。");
        if (s.Length is <= 0 or > 16777216) throw new IOException("内置存档大小异常。");
        byte[] bytes = new byte[checked((int)s.Length)]; s.ReadExactly(bytes); return bytes;
    }
    private Dictionary<string, byte[]> Template(string mode, string runtimeRoot)
    {
        var names = Names(mode);
        Dictionary<string, byte[]> result;
        if (TemplateOverride != null) result = TemplateOverride(mode);
        else
        {
            using var doc = JsonDocument.Parse(Resource("manifest.json"));
            if (mode == "MOD")
            {
                string required = doc.RootElement.GetProperty("ModRuntimeSha256").GetString()!;
                string dll = Path.Combine(runtimeRoot, "scbl_gameplay.dll");
                if (!File.Exists(dll) || !GameRuntime.Hash(dll).Equals(required, StringComparison.OrdinalIgnoreCase))
                    throw new IOException("此 MOD 存档模板与当前玩法 DLL 版本不匹配，未覆盖存档。请使用配套完整包。");
            }
            result = new(StringComparer.Ordinal);
            foreach (string name in names)
            {
                byte[] bytes = Resource(mode + "." + name);
                var row = doc.RootElement.GetProperty("Files").GetProperty(mode + "/" + name);
                if (bytes.Length != row.GetProperty("bytes").GetInt32() || !Hash(bytes).Equals(row.GetProperty("sha256").GetString(), StringComparison.OrdinalIgnoreCase))
                    throw new IOException("内置存档完整性校验失败。");
                result.Add(name, bytes);
            }
        }
        if (result.Count != names.Length || !names.All(result.ContainsKey) || result.Values.Any(b => b.Length is <= 0 or > 16777216))
            throw new IOException("存档模板文件集合无效。");
        return result;
    }
    private static void Plain(string path)
    {
        for (string? p = Path.GetFullPath(path); p != null; p = Path.GetDirectoryName(p))
            if ((File.Exists(p) || Directory.Exists(p)) && (File.GetAttributes(p) & FileAttributes.ReparsePoint) != 0)
                throw new IOException("存档路径含链接或目录联接，未覆盖。");
    }
    private static byte[]? Existing(string path)
    {
        Plain(path);
        if (Directory.Exists(path)) throw new IOException("存档文件路径被目录占用。");
        if (!File.Exists(path)) return null;
        if ((File.GetAttributes(path) & FileAttributes.ReadOnly) != 0) throw new IOException("存档文件为只读，未覆盖。");
        if (new FileInfo(path).Length > 16777216) throw new IOException("现有存档文件过大，未覆盖。");
        using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.None);
        byte[] bytes = new byte[checked((int)stream.Length)]; stream.ReadExactly(bytes); return bytes;
    }
    private static void Write(string path, byte[] bytes, bool overwrite)
    {
        Plain(path); string temp = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            using (var stream = new FileStream(temp, FileMode.CreateNew, FileAccess.Write, FileShare.None)) { stream.Write(bytes); stream.Flush(true); }
            if (GameRuntime.Hash(temp) != Hash(bytes)) throw new IOException("存档写入校验失败。");
            File.Move(temp, path, overwrite);
        }
        finally { if (File.Exists(temp)) File.Delete(temp); }
    }
    public string RestoreBundled(string mode, string runtimeRoot)
    {
        string directory = DirectoryFor(mode);
        if (running()) throw new IOException("请先在游戏内保存并退出游戏，再恢复存档。");
        var template = Template(mode, runtimeRoot); // Validate every resource before touching player files.
        Plain(directory); Plain(backups);
        using var mutex = new Mutex(false, "Local\\SCBL_LAN_Saves_" + Hash(Encoding.UTF8.GetBytes(SaveDirectory.ToUpperInvariant())));
        bool owns = false;
        try
        {
            try { owns = mutex.WaitOne(0); } catch (AbandonedMutexException) { owns = true; }
            if (!owns) throw new IOException("另一个存档操作正在进行，请稍后重试。");
            var originals = template.Keys.ToDictionary(n => n, n => Existing(Path.Combine(directory, n)));
            string backup = Path.Combine(backups, mode, DateTime.UtcNow.ToString("yyyyMMdd-HHmmss") + "-" + Guid.NewGuid().ToString("N"));
            Plain(backup); Directory.CreateDirectory(backup);
            foreach (var row in originals) if (row.Value != null) Write(Path.Combine(backup, row.Key), row.Value, false);
            var ledger = originals.ToDictionary(p => p.Key, p => new { existed = p.Value != null, sha256 = p.Value == null ? null : Hash(p.Value) });
            void Journal(string state) => Write(Path.Combine(backup, "manifest.json"), JsonSerializer.SerializeToUtf8Bytes(new { mode, state, destination = directory, files = ledger }), true);
            Journal("Prepared");
            var committed = new List<string>();
            try
            {
                Directory.CreateDirectory(directory);
                int index = 0;
                foreach (var row in template)
                {
                    BeforeWrite?.Invoke(index++);
                    if (running()) throw new IOException("游戏已启动，停止恢复存档。");
                    string target = Path.Combine(directory, row.Key); byte[]? actual = Existing(target), before = originals[row.Key];
                    if ((actual == null) != (before == null) || actual != null && !actual.AsSpan().SequenceEqual(before))
                        throw new IOException("存档在确认后被修改，已停止覆盖。");
                    Write(target, row.Value, before != null); committed.Add(row.Key);
                }
                foreach (var row in template) if (GameRuntime.Hash(Path.Combine(directory, row.Key)) != Hash(row.Value)) throw new IOException("恢复结果校验失败。");
                Journal("Complete"); return backup;
            }
            catch (Exception originalError)
            {
                bool recovered = true;
                foreach (string name in committed.AsEnumerable().Reverse())
                {
                    try
                    {
                        string target = Path.Combine(directory, name); Plain(target);
                        // Do not clobber game/concurrent writes while rolling back.
                        if (running() || !File.Exists(target) || GameRuntime.Hash(target) != Hash(template[name])) { recovered = false; continue; }
                        if (originals[name] is { } bytes) Write(target, bytes, true); else File.Delete(target);
                    }
                    catch { recovered = false; }
                }
                try { Journal(recovered ? "RolledBack" : "ManualRecoveryRequired"); } catch { recovered = false; }
                throw new IOException((recovered ? "恢复未完成，已回滚本次写入。备份：" : "恢复中断，存在外部改动或文件占用，请从备份检查恢复：") + backup, originalError);
            }
        }
        finally { if (owns) mutex.ReleaseMutex(); }
    }
}

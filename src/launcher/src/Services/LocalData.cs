using System.Text.Json;

namespace ScblLan;

public sealed record Settings(string Server = "", string GameExecutable = "", string BindOverride = "", string Username = "");

public sealed class LocalData
{
    public string Root { get; }
    public string LogPath => Path.Combine(Root, "logs", "launcher.log");
    public static string SaveDirectory => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "5th-Echelon", "Saves");
    public LocalData(string? programRoot = null)
    {
        Root = Path.Combine(programRoot ?? AppContext.BaseDirectory, "data", Segment(Environment.MachineName), Segment(Environment.UserName));
        Directory.CreateDirectory(Path.Combine(Root, "logs"));
    }
    public static string Segment(string value)
    {
        var result = string.Concat(value.Select(c => char.IsControl(c) || Path.GetInvalidFileNameChars().Contains(c) ? '_' : c)).Trim().TrimEnd('.');
        return string.IsNullOrEmpty(result) || result is "." or ".." ? "UNKNOWN" : result;
    }
    public Settings Load()
    {
        string p = Path.Combine(Root, "settings.json");
        if (!File.Exists(p)) return new();
        try { return JsonSerializer.Deserialize<Settings>(File.ReadAllText(p)) ?? new(); }
        catch (JsonException) { Log("设置文件无法解析；保留原文件并使用空设置。"); return new(); }
    }
    public void Save(Settings settings) => AtomicWrite(Path.Combine(Root, "settings.json"), JsonSerializer.Serialize(settings, new JsonSerializerOptions { WriteIndented = true }));
    public void Log(string message) => File.AppendAllText(LogPath, $"{DateTimeOffset.Now:O} {message}{Environment.NewLine}");
    public static void AtomicWrite(string path, string text)
    {
        string temp = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try { File.WriteAllText(temp, text, new System.Text.UTF8Encoding(false)); File.Move(temp, path, true); }
        finally { if (File.Exists(temp)) File.Delete(temp); }
    }
}

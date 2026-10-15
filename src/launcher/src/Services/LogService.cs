using System;
using System.IO;
using System.Text;

namespace SplinterCellCNLauncher.Services;

public static class LogService
{
    public static string AppDataDir { get; } = Path.Combine(AppContext.BaseDirectory, "data", ScblLan.LocalData.Segment(Environment.MachineName), ScblLan.LocalData.Segment(Environment.UserName));

    public static string LogPath { get; } = Path.Combine(AppDataDir, "launcher.log");

    public static void Info(string message)
    {
        Write("INFO", message);
    }

    public static void Error(string message)
    {
        Write("ERROR", message);
    }

    public static void Error(Exception ex)
    {
        DiagnosticEvents.RecordException(ex);
        Write("ERROR", ex.ToString());
    }

    private static void Write(string level, string message)
    {
        DiagnosticEvents.Record(level,message);
        try
        {
            Directory.CreateDirectory(AppDataDir);
            string line = $"[{DateTime.Now:yyyy-MM-dd HH:mm:ss}] [{level}] {message}{Environment.NewLine}";
            File.AppendAllText(LogPath, line, Encoding.UTF8);
        }
        catch
        {
            // 日志失败不应该影响启动器主流程
        }
    }
}

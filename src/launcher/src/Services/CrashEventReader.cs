using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.Runtime.InteropServices;
using System.Xml;
using System.Xml.Linq;

namespace SplinterCellCNLauncher.Services;

public sealed record CrashQueryContext(string ExecutablePath, int Pid, DateTimeOffset CreatedUtc, DateTimeOffset ExitUtc);
public sealed record CrashReadResult(CrashQueryState State, int? Win32Error, IReadOnlyList<string> Xml);
public interface ICrashEventSource { CrashReadResult Read(CrashQueryContext context); }

public sealed class WindowsCrashEventSource : ICrashEventSource
{
    public CrashReadResult Read(CrashQueryContext c)
    {
        var rows = new List<string>(); IntPtr query = IntPtr.Zero;
        try {
            // Event time and provider filters are generated only from typed values, never user input.
            string from = c.CreatedUtc.AddSeconds(-2).UtcDateTime.ToString("yyyy-MM-ddTHH:mm:ss.fffZ", CultureInfo.InvariantCulture);
            string to = c.ExitUtc.AddMinutes(2).UtcDateTime.ToString("yyyy-MM-ddTHH:mm:ss.fffZ", CultureInfo.InvariantCulture);
            string xpath = $"*[System[((EventID=1000 and Provider[@Name='Application Error']) or (EventID=1001 and Provider[@Name='Windows Error Reporting'])) and TimeCreated[@SystemTime>='{from}' and @SystemTime<='{to}']] and (EventData[Data[@Name='AppName']='Blacklist_DX11_game.exe'] or EventData[Data[@Name='P1']='Blacklist_DX11_game.exe'])]";
            query = EvtQuery(IntPtr.Zero, "Application", xpath, 0x201);
            if (query == IntPtr.Zero) return Error(Marshal.GetLastWin32Error());
            var elapsed = Stopwatch.StartNew();
            while (rows.Count < 128 && elapsed.Elapsed < TimeSpan.FromSeconds(2)) {
                var handles = new IntPtr[1];
                if (!EvtNext(query, 1, handles, 100, 0, out int returned)) {
                    int error = Marshal.GetLastWin32Error();
                    return error == 259 ? new(CrashQueryState.NoCorrelatedEventYet, null, rows) : Error(error);
                }
                if (returned != 1) break;
                try {
                    EvtRender(IntPtr.Zero, handles[0], 1, 0, IntPtr.Zero, out int size, out _);
                    if (Marshal.GetLastWin32Error() != 122 || size <= 0 || size > 65536) continue;
                    IntPtr buffer = Marshal.AllocHGlobal(size);
                    try { if (EvtRender(IntPtr.Zero, handles[0], 1, size, buffer, out _, out _)) rows.Add(Marshal.PtrToStringUni(buffer) ?? ""); }
                    finally { Marshal.FreeHGlobal(buffer); }
                } finally { EvtClose(handles[0]); }
            }
            return new(CrashQueryState.Truncated, null, rows);
        } catch (Exception e) when (e is DllNotFoundException or EntryPointNotFoundException or UnauthorizedAccessException or Win32Exception) {
            return new(e is UnauthorizedAccessException ? CrashQueryState.AccessDenied : CrashQueryState.Unavailable, e.HResult, rows);
        } finally { if (query != IntPtr.Zero) EvtClose(query); }
        CrashReadResult Error(int e) => new(e == 5 ? CrashQueryState.AccessDenied : e == 1460 ? CrashQueryState.TimedOut : CrashQueryState.Unavailable, e, rows);
    }
    [DllImport("wevtapi.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern IntPtr EvtQuery(IntPtr session, string path, string query, int flags);
    [DllImport("wevtapi.dll", SetLastError = true)] private static extern bool EvtNext(IntPtr query, int size, [Out] IntPtr[] events, int timeout, int flags, out int returned);
    [DllImport("wevtapi.dll", SetLastError = true)] private static extern bool EvtRender(IntPtr context, IntPtr handle, int flags, int size, IntPtr buffer, out int used, out int count);
    [DllImport("wevtapi.dll")] private static extern bool EvtClose(IntPtr handle);
}

public static class CrashEventCorrelation
{
    // Event 1001 often has no PID/creation time. Only accept it by joining a strongly matched 1000 report ID.
    public static List<CrashEventSummary> Match(CrashQueryContext c, IEnumerable<string> xml, IReadOnlyList<CrashEventSummary>? previous = null)
    {
        var parsed = new List<(int Id, DateTimeOffset Utc, Dictionary<string, string> Data)>();
        foreach (string text in xml.Take(128)) {
            if (text.Length > 65536) continue;
            try {
                using var reader = XmlReader.Create(new StringReader(text), new XmlReaderSettings { DtdProcessing = DtdProcessing.Prohibit, XmlResolver = null, MaxCharactersInDocument = 65536 });
                var doc = XDocument.Load(reader); var sys = doc.Root?.Elements().FirstOrDefault(e => e.Name.LocalName == "System");
                string? Value(string n) => sys?.Elements().FirstOrDefault(e => e.Name.LocalName == n)?.Value;
                if (!int.TryParse(Value("EventID"), out int id) || id is not (1000 or 1001)) continue;
                string? provider = sys?.Elements().FirstOrDefault(e => e.Name.LocalName == "Provider")?.Attribute("Name")?.Value;
                if (provider != (id == 1000 ? "Application Error" : "Windows Error Reporting")) continue;
                if (!DateTimeOffset.TryParse(sys?.Elements().FirstOrDefault(e => e.Name.LocalName == "TimeCreated")?.Attribute("SystemTime")?.Value, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var utc)) continue;
                if (utc < c.CreatedUtc.AddSeconds(-2) || utc > c.ExitUtc.AddMinutes(2)) continue;
                var values = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
                foreach (var element in doc.Descendants().Where(e => e.Name.LocalName == "Data")) {
                    string? key = element.Attribute("Name")?.Value;
                    if (key != null && key.Length < 64 && element.Value.Length < 8192) values[key] = element.Value;
                }
                parsed.Add((id, utc, values));
            } catch (Exception e) when (e is XmlException or InvalidOperationException or ArgumentException) { }
        }
        var result = (previous ?? []).Where(e => e.EventId is 1000 or 1001 && e.Pid == c.Pid).Take(8).ToList();
        foreach (var item in parsed.Where(p => p.Id == 1000)) {
            var d = item.Data;
            string S(string key) => d.GetValueOrDefault(key, "");
            if (!Number(S("ProcessId"), out ulong pid) || pid != (ulong)c.Pid || !Number(S("ProcessCreationTime"), out ulong start)) continue;
            if (start != (ulong)c.CreatedUtc.UtcDateTime.ToFileTimeUtc()) continue;
            try { if (!Path.GetFullPath(S("AppPath")).Equals(Path.GetFullPath(c.ExecutablePath), StringComparison.OrdinalIgnoreCase)) continue; }
            catch (Exception e) when (e is ArgumentException or NotSupportedException or PathTooLongException) { continue; }
            result.Add(new(1000, item.Utc, c.Pid, "ExactPath+PID+CreationTime+TimeWindow", DiagnosticSafety.Module(S("ModuleName")), DiagnosticSafety.Version(S("ModuleVersion")), DiagnosticSafety.Hex(S("ExceptionCode"), 18), DiagnosticSafety.Hex(S("FaultingOffset"), 18), Guid.TryParse(S("IntegratorReportId"), out var guid) ? guid.ToString("D") : null));
        }
        foreach (var item in parsed.Where(p => p.Id == 1001)) {
            var d = item.Data; string S(string key) => d.GetValueOrDefault(key, "");
            if (S("EventName") != "APPCRASH" || !S("P1").Equals(Path.GetFileName(c.ExecutablePath), StringComparison.OrdinalIgnoreCase) || !Guid.TryParse(S("ReportId"), out var report)) continue;
            if (!result.Any(e => e.EventId == 1000 && e.ReportId == report.ToString("D"))) continue;
            result.Add(new(1001, item.Utc, c.Pid, "Matched1000ReportId+AppName+TimeWindow", DiagnosticSafety.Module(S("P4")), DiagnosticSafety.Version(S("P5")), DiagnosticSafety.Hex(S("P7"), 18), DiagnosticSafety.Hex(S("P8"), 18), report.ToString("D")));
        }
        return result.Distinct().OrderBy(e=>e.Utc).ThenBy(e=>e.EventId).Take(8).ToList();
    }
    private static bool Number(string text, out ulong value) => text.StartsWith("0x", StringComparison.OrdinalIgnoreCase)
        ? ulong.TryParse(text.AsSpan(2), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out value)
        : ulong.TryParse(text, NumberStyles.Integer, CultureInfo.InvariantCulture, out value);
}

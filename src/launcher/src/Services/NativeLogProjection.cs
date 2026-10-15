using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace SplinterCellCNLauncher.Services;

// Read only the two logs produced under this newly created run directory. Export known stage IDs, never lines.
internal sealed class NativeLogProjection(string dataRoot, Action<RunStage, int?> record, Action<RunEvent>? detail = null) : IDisposable
{
    private readonly string path = Path.Combine(dataRoot, "logs", "game", "bl-tracing.log");
    private readonly string party = Path.Combine(dataRoot, "logs", "game", "hooks-party-trace.log");
    private FileStream? stream;
    private ulong identity;
    private readonly StringBuilder line = new();
    private bool oversized, warned;
    public void Poll()
    {
        try {
            if (!DiagnosticSafety.Plain(path)) { Warn(); return; }
            if (File.Exists(path)) {
                using var probe = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
                ulong id = Identity(probe);
                if (stream == null || identity != id) {
                    if (stream != null) { Drain(); stream.Dispose(); record(RunStage.LogReplacedOrTruncated, null); }
                    stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
                    identity = Identity(stream); line.Clear(); oversized = false;
                }
                if (stream.Length < stream.Position) { stream.Position = 0; line.Clear(); oversized = false; record(RunStage.LogReplacedOrTruncated, null); }
                if (stream.Length-stream.Position>8L*1024*1024) { stream.Position=stream.Length-256*1024;line.Clear();oversized=true;record(RunStage.LogCapacityTrim,null); }
                Drain();
                // Never truncate a file while the native writer is using it. Only our bounded
                // projection ring is retained during a run. Closed oversized raw files are trimmed separately.
            }
        } catch (Exception e) when (e is IOException or UnauthorizedAccessException or ObjectDisposedException) { Warn(); }
    }
    private void Warn() { if (!warned) { warned = true; record(RunStage.LogUnavailable, null); } }
    private void Drain()
    {
        if (stream == null) return;
        byte[] buffer = new byte[65536];
        int remaining = 256 * 1024;
        while (remaining > 0) {
            int read = stream.Read(buffer, 0, Math.Min(buffer.Length, remaining)); if (read == 0) break; remaining -= read;
            // All accepted markers are ASCII. Non-ASCII bytes cannot become exported text.
            for (int i = 0; i < read; i++) {
                char c = buffer[i] < 128 ? (char)buffer[i] : '?';
                if (c == '\n') { if (!oversized) ProjectDetailed(line.ToString(), record, detail); line.Clear(); oversized = false; }
                else if (line.Length < 8192) line.Append(c); else oversized = true;
            }
        }
    }
    internal static void Project(string line, Action<RunStage, int?> emit)
    {
        if (line.Length > 8192) return;
        if (line.Contains("attaching")) emit(RunStage.HookAttached, null);
        if (line.Contains("SCBL_GAMEPLAY_PLUGIN initialized")) emit(RunStage.GameplayInitialized, null);
        if (line.Contains("unlock_catalog")) emit(RunStage.UnlockCatalog, null);
        if (line.Contains("SCBL_EXTENDED_LOADOUTS event=clone")) emit(RunStage.LoadoutClone, null);
        if (line.Contains("SCBL_EXTENDED_LOADOUTS event=serialize")) emit(RunStage.LoadoutSerialize, null);
        if (line.Contains("SCBL_FUSION_COMPAT duplicate_modules=true")) emit(RunStage.DuplicateFusionModules, null);
        if (line.Contains("Panic occurred in file")) emit(RunStage.NativePanic, null);
        if (line.Contains(" ERROR ")) emit(RunStage.NativeError, null);
    }
    internal static void ProjectDetailed(string text, Action<RunStage, int?> emit, Action<RunEvent>? details)
    {
        if (ProjectExposure(text) is { } exposure) {
            if (details == null) emit(exposure.Stage, null); else details(exposure);
        }
        Project(text, (stage, pid) => {
            if (details == null) { emit(stage, pid); return; }
            var ts = System.Text.RegularExpressions.Regex.Match(text, @"\A(\d{4}-\d{2}-\d{2}T[0-9:.]+Z)");
            DateTimeOffset? source = DateTimeOffset.TryParse(ts.Value, System.Globalization.CultureInfo.InvariantCulture, System.Globalization.DateTimeStyles.AssumeUniversal, out var time) ? time : null;
            string Field(string name) { var m = System.Text.RegularExpressions.Regex.Match(text, @"(?:^|\s)" + name + @"=([A-Za-z0-9-]+)(?:\s|$)"); return m.Success ? m.Groups[1].Value : "Unknown"; }
            int? Number(string name) => int.TryParse(Field(name), out int v) ? v : null;
            string phase = Field("phase"); if (phase == "Unknown" && stage is RunStage.LoadoutClone or RunStage.LoadoutSerialize) phase = Field("event");
            if (phase == "Unknown" && stage == RunStage.GameplayInitialized) phase = "init";
            details(DiagnosticSafety.CleanStage(new(DateTimeOffset.UtcNow, stage, pid, source, phase, Field("result"), Number("items") ?? Number("added"), Number("revision"))));
        });
    }

    // Project a finite set of diagnostic facts, never arbitrary text, names, IDs or addresses.
    // These events describe local observations; none authorizes gameplay or proves remote delivery.
    internal static RunEvent? ProjectExposure(string text)
    {
        if (text.Length > 8192) return null;
        var marker = System.Text.RegularExpressions.Regex.Match(text, @"(?:^|\s)SCBL_MOD_EXPOSURE_(BRIDGE|FRAME|UI)(?:\s|$)");
        if (!marker.Success) return null;
        string body = text[(marker.Index + marker.Length)..];
        string Field(string name) {
            var matches = System.Text.RegularExpressions.Regex.Matches(body, @"(?:^|\s)" + name + @"=([^\s]+)(?=\s|$)");
            if (matches.Count != 1) return "Unknown";
            string value = matches[0].Groups[1].Value;
            // Structured tracing string fields may be quoted. Only a matching
            // outer pair is removed; the finite allowlists still decide admission.
            return value.Length >= 2 && value[0] == '"' && value[^1] == '"' ? value[1..^1] : value;
        }
        bool? Flag(string name) => Field(name) switch { "true" => true, "false" => false, _ => null };
        RunEvent? value = null;
        var now = DateTimeOffset.UtcNow;
        if (marker.Groups[1].Value == "BRIDGE") {
            string source = Field("source");
            if (source is "RFD" or "ATS" or "BMS") {
                // Reject ambiguous success/rejection combinations rather than inventing a result.
                bool sent = Field("native_send_accepted") == "true", received = Field("native_receipt_valid") == "true";
                string rejection = Field("receipt_rejected");
                if ((sent ? 1 : 0) + (received ? 1 : 0) + (rejection != "Unknown" ? 1 : 0) != 1) return null;
                value = sent ? new(now, RunStage.ExposureSent, Phase: source, Result: "accepted")
                    : received ? new(now, RunStage.ExposureReceived, Phase: source, Result: "valid")
                    : new(now, RunStage.ExposureRejected, Phase: source, Result: rejection);
            } else if (Field("receipt_rejected") == "current_native_frame") value = new(now, RunStage.ExposureContextRejected, Phase: "receive", Result: "current_native_frame");
            else if (Field("send_skipped") == "current_native_frame") value = new(now, RunStage.ExposureContextRejected, Phase: "send", Result: "current_native_frame");
        } else if (marker.Groups[1].Value == "FRAME") {
            if (!body.StartsWith("rejected_native_context ", StringComparison.Ordinal)) return null;
            value = new(now, RunStage.ExposureContextRejected, Phase: "frame", Result: Field("reason"));
        } else if (Field("kind") == "Exposed") {
            bool? applied = Flag("applied"); if (applied == null) return null;
            bool? marked = Field("native_marked") switch { "Some(true)" => true, "Some(false)" => false, _ => null };
            value = new(now, RunStage.ExposureUi, Phase: Field("state"), Result: applied.Value ? "applied" : "not-applied", Alive: Flag("alive"), GogglesOn: Flag("goggles_on"), NativeMarked: marked);
        }
        if (value == null) return null;
        var ts = System.Text.RegularExpressions.Regex.Match(text, @"\A(\d{4}-\d{2}-\d{2}T[0-9:.]+Z)");
        DateTimeOffset? sourceTime = DateTimeOffset.TryParse(ts.Value, System.Globalization.CultureInfo.InvariantCulture, System.Globalization.DateTimeStyles.AssumeUniversal, out var time) ? time : null;
        value = DiagnosticSafety.CleanStage(value with { SourceUtc = sourceTime });
        return value.Phase == "Unknown" || value.Result == "Unknown" ? null : value;
    }

    private static ulong Identity(FileStream value) => GetFileInformationByHandle(value.SafeFileHandle, out var info) ? ((ulong)info.IndexHigh << 32) | info.IndexLow : throw new IOException("Log identity unavailable.");
    [StructLayout(LayoutKind.Sequential, Pack = 4)] private struct Information { public uint Attributes; public long Creation, Access, Write; public uint Volume, SizeHigh, SizeLow, Links, IndexHigh, IndexLow; }
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool GetFileInformationByHandle(SafeFileHandle file, out Information info);
    public void Dispose() => stream?.Dispose();
}

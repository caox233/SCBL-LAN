using System.Text.Json;
using System.Text.Json.Serialization;
using System.Text.RegularExpressions;

namespace SplinterCellCNLauncher.Services;

public enum RunIntent { None, UserEnd, UserRestart, Cancelled, LauncherClosed, LaunchFailed }
public enum RunOutcome { Starting, Running, ConfirmedCrash, AwaitingHandoff, EarlyExitUnconfirmed, NormalExit, NonzeroExit, UserEnd, UserRestart, Cancelled, LauncherClosed, LaunchFailed, ObservationUnavailable }
public enum ProcessEvidence { CreatedByLauncher, ValidatedConfigurationPeer }
public enum RunStage { Prepared, ProcessCreated, ConfigurationPeer, ConfigurationDelivered, Stable, ProcessExited, HookAttached, GameplayInitialized, UnlockCatalog, LoadoutClone, LoadoutSerialize, DuplicateFusionModules, NativePanic, NativeError, LogReplacedOrTruncated, LogCapacityTrim, LogUnavailable, UserAction, ObservationUnavailable, ExposureSent, ExposureReceived, ExposureRejected, ExposureContextRejected, ExposureUi }
public enum CrashQueryState { NotRequested, Pending, Matched, NoCorrelatedEventYet, NoCorrelatedEventWithinWindow, AccessDenied, Unavailable, TimedOut, Truncated }
public enum FingerprintState { Read, Missing, Unavailable, TooLarge, ReparsePointSkipped }
public sealed record RunEvent(DateTimeOffset ObservedUtc, RunStage Stage, int? Pid = null,
    DateTimeOffset? SourceUtc = null, string Phase = "Unknown", string Result = "Unknown", int? Count = null, int? Revision = null, bool? Alive = null, bool? GogglesOn = null, bool? NativeMarked = null);
public enum RunGameMode { Unknown, Official, MOD }
public enum ModuleSource { Unknown, GameDirectory, GameScripts, PackagedRuntime, Windows, Other }
public sealed record RunLaunchProfile(RunGameMode Mode, string Graphics = "DX11", string Generation = "0.4.0", ModuleSource RequestedSource = ModuleSource.Unknown);
public sealed record LoadedModuleObservation(int Pid, DateTimeOffset CreatedUtc, DateTimeOffset ObservedUtc, string Name, ModuleSource Source, FingerprintState HashState, string? Sha256);
public enum CaptureState { Disabled, Preparing, Armed, Captured, Failed, Expired, Detached, Exited, LimitReached }
public sealed record CaptureObservation(int Pid, DateTimeOffset Utc, CaptureState State, string Stage, int? Error = null);
public sealed record LauncherRunEvent(DateTimeOffset Utc, string Level, string Code, string? ExceptionType, int? HResult, int? SocketError);
public sealed record ModuleFingerprint(string Name, FingerprintState Status, long? Bytes, string? Sha256, string? Version);
public sealed record CrashEventSummary(int EventId, DateTimeOffset Utc, int Pid, string Correlation, string Module, string? ModuleVersion, string? ExceptionCode, string? Offset, string? ReportId);
public sealed class ProcessRunSummary
{
    public int Pid { get; set; }
    public DateTimeOffset CreatedUtc { get; set; }
    public ProcessEvidence Evidence { get; set; }
    public DateTimeOffset? ExitUtc { get; set; }
    public uint? ExitCodeDecimal { get; set; }
    public string? ExitCodeHex => ExitCodeDecimal is uint value ? $"0x{value:X8}" : null;
    public bool ExitObserved { get; set; }
    public bool ObservationUnavailable { get; set; }
    public bool SuccessorObserved { get; set; }
    public CrashQueryState WerState { get; set; }
    public int? WerWin32Error { get; set; }
    public int WerAttempts { get; set; }
    public List<CrashEventSummary> Faults { get; set; } = [];
}
public sealed record RequestedConfigurationStatus(string State,int? Schema=null,long? Bytes=null,string? Sha256=null,DateTimeOffset? RecordedUtc=null,string? ErrorCode=null);
public sealed class RunSummary
{
    public int Schema { get; set; } = 2;
    public Guid RunId { get; set; }
    public DateTimeOffset StartedUtc { get; set; }
    public DateTimeOffset UpdatedUtc { get; set; }
    public string Executable { get; set; } = "Blacklist_DX11_game.exe";
    public string? ExecutableSha256 { get; set; }
    public string? ExecutablePathSha256 { get; set; }
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public RequestedConfigurationStatus? EffectiveConfigurationSnapshot { get; set; }
    public RunIntent Intent { get; set; }
    public DateTimeOffset? IntentUtc { get; set; }
    public RunLaunchProfile LaunchProfile { get; set; } = new(RunGameMode.Unknown);
    public bool GameplayRequested { get; set; }
    public string LoadedModulesState { get; set; } = "NotObserved";
    public List<LoadedModuleObservation> LoadedModules { get; set; } = [];
    public List<CaptureObservation> Capture { get; set; } = [];
    public RunOutcome Outcome { get; set; }
    public bool StableObserved { get; set; }
    public bool SessionClosed { get; set; }
    public bool DuplicateFusionFiles { get; set; }
    public bool DuplicateFusionModulesObserved { get; set; }
    public List<ProcessRunSummary> Processes { get; set; } = [];
    public List<RunEvent> Stages { get; set; } = [];
    public List<LauncherRunEvent> LauncherEvents { get; set; } = [];
    public string ComponentEvidence => "DiskPresenceOnlyNotLoadEvidence";
    public string GameplayLoadState => LoadedModules.Any(m=>m.Name.Equals("scbl_gameplay.dll",StringComparison.OrdinalIgnoreCase)) ? "ObservedLoaded" : !GameplayRequested ? "NotRequested" : LoadedModulesState is "Unavailable" or "NotObserved" ? "ObservationUnavailable" : "NotObserved";
    public List<ModuleFingerprint> Components { get; set; } = [];
}

internal static class DiagnosticSafety
{
    internal static readonly JsonSerializerOptions Json = new() { WriteIndented = true, Converters = { new JsonStringEnumConverter() } };
    internal static readonly string[] Modules = ["Blacklist_DX11_game.exe", "uplay_r1_loader.dll", "uplay_r1_loader.orig.dll", "version.dll", "SplinterCellBlacklist.FusionFix.asi", "SplinterCellBlacklist.FusionMod.asi", "scbl_gameplay.dll"];
    internal static string? Hex(string? s, int max = 64) => s != null && s.Length <= max && Regex.IsMatch(s, @"\A(?:0x)?[0-9A-Fa-f]+\z") ? s : null;
    internal static string? Version(string? s) => s != null && Regex.IsMatch(s, @"\A[0-9]{1,10}(?:\.[0-9]{1,10}){1,3}\z") ? s : null;
    internal static string Module(string s)
    {
        string name = Path.GetFileName(s);
        return Regex.IsMatch(name, @"\A[A-Za-z0-9_.-]{1,120}\.(?:dll|exe|asi|addon32|addon64)\z", RegexOptions.IgnoreCase) ? name : "UnknownModule";
    }
    internal static string CaptureStage(string s) => new[] { "Prepare", "Connect", "Identity", "Attach", "KillOnExit", "Ready", "Context", "Dump", "Captured", "Wait", "Continue", "Detach", "Exit", "Timeout", "ParentClosed", "Limit", "Helper", "Export" }.Contains(s) ? s : "Helper";
    internal static bool ExposureStage(RunStage stage) => stage is RunStage.ExposureSent or RunStage.ExposureReceived or RunStage.ExposureRejected or RunStage.ExposureContextRejected or RunStage.ExposureUi;
    internal static RunEvent CleanStage(RunEvent e)
    {
        if (ExposureStage(e.Stage)) {
            bool ui = e.Stage == RunStage.ExposureUi;
            string[] phases = e.Stage == RunStage.ExposureContextRejected ? ["frame", "send", "receive"] : ui ? ["Exposed", "Hidden", "Native"] : ["RFD", "ATS", "BMS"];
            string[] results = e.Stage switch {
                RunStage.ExposureSent => ["accepted"], // Local send result, not remote delivery.
                RunStage.ExposureReceived => ["valid"],
                RunStage.ExposureRejected => ["invalid_context", "invalid_report", "different_match", "source_owner_or_role", "target_role_team_or_roster", "stale_or_duplicate_sequence", "receipt_capacity"],
                RunStage.ExposureContextRejected => ["current_native_frame", "missing_information", "missing_world", "missing_world_info", "missing_manager", "missing_network_object", "network_class_or_backref", "missing_session", "invalid_session_peers", "invalid_match_identity", "invalid_local_controller", "invalid_native_roster", "invalid_actor_pointer", "actor_class_or_binding", "actor_peer_not_in_session", "invalid_actor_identity", "missing_local_identity", "invalid_world_time", "invalid_context_contract"],
                RunStage.ExposureUi => ["applied", "not-applied"],
                _ => []
            };
            return e with { Pid = null, Phase = phases.Contains(e.Phase) ? e.Phase : "Unknown", Result = results.Contains(e.Result) ? e.Result : "Unknown",
                Count = null, Revision = null, Alive = ui ? e.Alive : null, GogglesOn = ui ? e.GogglesOn : null, NativeMarked = ui ? e.NativeMarked : null };
        }
        return e with {
            Phase = new[] { "init", "after-load", "armor-after-load", "camo-status", "clone", "serialize", "serialize-read", "serialize-write" }.Contains(e.Phase) ? e.Phase : "Unknown",
            Result = new[] { "added", "failed", "missing-source", "ok", "rejected", "skipped", "enabled" }.Contains(e.Result) ? e.Result : "Unknown",
            Count = e.Count is >= 0 and <= 1000000 ? e.Count : null, Revision = e.Revision is >= 0 and <= 100000 ? e.Revision : null,
            Alive = null, GogglesOn = null, NativeMarked = null };
    }
    internal static bool Plain(string path)
    {
        for (string? p = Path.GetFullPath(path); p != null; p = Path.GetDirectoryName(p))
            if ((File.Exists(p) || Directory.Exists(p)) && (File.GetAttributes(p) & FileAttributes.ReparsePoint) != 0) return false;
        return true;
    }
    internal static LauncherRunEvent Clean(LauncherRunEvent e) => new(e.Utc, e.Level == "ERROR" ? "ERROR" : "INFO",
        new[] { "authentication", "configuration", "discovery", "broadcast", "game-lifecycle", "server", "credential-store", "launcher", "exception" }.Contains(e.Code) ? e.Code : "launcher",
        new[] { "IOException", "UnauthorizedAccessException", "SocketException", "InvalidOperationException", "OperationCanceledException", "TaskCanceledException", "Win32Exception" }.Contains(e.ExceptionType) ? e.ExceptionType : null, e.HResult, e.SocketError);
    // Persistent files are untrusted too. Never serialize arbitrary strings from a local JSON file.
    internal static RequestedConfigurationStatus? Clean(RequestedConfigurationStatus? s)
    {
        if(s==null)return null;
        string state=new[]{"Written","ProjectionUnavailable","WriteFailed","Conflict","ReparsePointSkipped"}.Contains(s.State)?s.State:"ProjectionUnavailable";
        if(state=="Written" && s.Schema==1 && s.Bytes is >0 and <=65536 && (s.Sha256==null || s.Sha256.Length==64 && s.Sha256.All(Uri.IsHexDigit)) && s.RecordedUtc!=null)
            return new(state,1,s.Bytes,s.Sha256?.ToUpperInvariant(),s.RecordedUtc);
        return new(state=="Written"?"ProjectionUnavailable":state,ErrorCode:RequestedConfigurationSnapshot.SafeError(s.ErrorCode));
    }
    // Consistency of a persisted claim only: this does not revalidate WER path/creation/time evidence.
    internal static string? FaultRejectionReason(CrashEventSummary e, int parentPid) =>
        e.EventId is not (1000 or 1001) ? "RejectedUnsupportedEvent" :
        parentPid <= 0 || e.Pid <= 0 ? "RejectedMissingOrInvalidPid" :
        e.Pid != parentPid ? "RejectedProcessPidMismatch" : null;
    internal static CrashEventSummary CleanFault(CrashEventSummary e, int parentPid, string? projectedModule = null) => e with {
        Correlation = FaultRejectionReason(e, parentPid) ?? (e.EventId == 1000 ? "ExactPath+PID+CreationTime+TimeWindow" : "Matched1000ReportId+AppName+TimeWindow"),
        Module = projectedModule ?? Module(e.Module ?? ""), ModuleVersion = Version(e.ModuleVersion), ExceptionCode = Hex(e.ExceptionCode, 18), Offset = Hex(e.Offset, 18),
        ReportId = Guid.TryParse(e.ReportId, out var id) ? id.ToString("D") : null };
    internal static RunSummary? Clean(RunSummary s)
    {
        if (s.Schema != 2 || s.RunId == Guid.Empty || !Enum.IsDefined(s.Intent) || !Enum.IsDefined(s.Outcome)) return null;
        // Legacy summary export must not publish an untrusted snapshot-source digest either.
        s.EffectiveConfigurationSnapshot=Clean(s.EffectiveConfigurationSnapshot) is {} snapshot ? snapshot with {Sha256=null} : null;
        s.Executable = "Blacklist_DX11_game.exe";
        s.ExecutableSha256 = s.ExecutableSha256?.Length == 64 ? Hex(s.ExecutableSha256) : null;
        s.ExecutablePathSha256 = s.ExecutablePathSha256?.Length == 64 ? Hex(s.ExecutablePathSha256) : null;
        s.Stages = (s.Stages ?? []).Where(e => Enum.IsDefined(e.Stage)).TakeLast(256).Select(CleanStage).ToList();
        s.LaunchProfile = new(Enum.IsDefined(s.LaunchProfile.Mode) ? s.LaunchProfile.Mode : RunGameMode.Unknown, "DX11", Version(s.LaunchProfile.Generation) ?? "0.4.0", Enum.IsDefined(s.LaunchProfile.RequestedSource) ? s.LaunchProfile.RequestedSource : ModuleSource.Unknown);
        s.LoadedModulesState = new[] { "NotObserved", "Observed", "Partial", "Unavailable" }.Contains(s.LoadedModulesState) ? s.LoadedModulesState : "Unavailable";
        s.LoadedModules = (s.LoadedModules ?? []).Where(m => m.Pid > 0 && Enum.IsDefined(m.Source) && Enum.IsDefined(m.HashState)).TakeLast(512)
            .Select(m => m with { Name = Module(m.Name ?? ""), Sha256 = m.Sha256?.Length == 64 ? Hex(m.Sha256) : null }).ToList();
        s.Capture = (s.Capture ?? []).Where(c => Enum.IsDefined(c.State)).TakeLast(32).Select(c => c with { Stage = CaptureStage(c.Stage) }).ToList();
        s.LauncherEvents = (s.LauncherEvents ?? []).TakeLast(200).Select(Clean).ToList();
        s.Components = (s.Components ?? []).Where(e => Modules.Contains(e.Name) && Enum.IsDefined(e.Status)).Take(Modules.Length)
            .Select(e => e with { Sha256 = e.Sha256?.Length == 64 ? Hex(e.Sha256) : null, Version = Version(e.Version) }).ToList();
        s.Processes = (s.Processes ?? []).Where(p => p.Pid > 0 && Enum.IsDefined(p.Evidence) && Enum.IsDefined(p.WerState)).Take(16).ToList();
        foreach (var p in s.Processes)
            p.Faults = (p.Faults ?? []).Where(e => FaultRejectionReason(e, p.Pid) == null).Take(8).Select(e => CleanFault(e, p.Pid)).ToList();
        return s;
    }
}

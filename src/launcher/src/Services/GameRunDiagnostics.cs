using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace SplinterCellCNLauncher.Services;

// Diagnostic sidecar only: never kills a process, changes game configuration, or decides launch success.
public sealed class GameRunDiagnostics
{
    private readonly object gate = new();
    private readonly object tickGate = new();
    private readonly string executable, directory;
    private readonly ICrashEventSource source;
    private readonly List<TrackedProcess> tracked = [];
    private readonly RunSummary summary;
    private readonly NativeLogProjection logs;
    private DateTimeOffset closedUtc;
    private bool stopped;
    private bool requestedConfigurationAttempted;
    internal void TryWriteRequestedConfiguration(RequestedConfigurationProjection projection)
    {
        lock(gate) {
            if(requestedConfigurationAttempted || closeRequested || stopped)return;
            requestedConfigurationAttempted=true;
            summary.EffectiveConfigurationSnapshot=RequestedConfigurationSnapshot.WriteOnce(directory,Id,projection);
            // Fixed local event only; global DiagnosticEvents would route a late A failure to Current B.
            if(summary.EffectiveConfigurationSnapshot.State!="Written") {
                summary.LauncherEvents.Add(new(DateTimeOffset.UtcNow,"ERROR","configuration",null,null,null));
                if(summary.LauncherEvents.Count>200)summary.LauncherEvents.RemoveAt(0);
            }
            Save();
        }
    }
    private bool closeRequested;
    private int ticks;
    private readonly Dictionary<string, ModuleFingerprint> moduleHashes = new(StringComparer.OrdinalIgnoreCase);
    private NativeCaptureSession? capture;
    public bool HasTerminalExit { get { lock(gate) return TerminalExit(summary); } }
    internal static bool TerminalExit(RunSummary s) => s.Processes.Count > 0 && s.Processes.All(p => p.ExitObserved || p.ObservationUnavailable) &&
        s.Processes.Any(p => p.ExitObserved && (p.Faults.Count > 0 || (p.ExitCodeDecimal is > 0 && p.Evidence == ProcessEvidence.ValidatedConfigurationPeer)));
    public void EnableCapture(string? helperOverride = null, int lifetimeSeconds = 600) {
        try { capture = new NativeCaptureSession(directory, executable, CaptureEvent, helperOverride, lifetimeSeconds); }
        catch(Exception e) { CaptureEvent(new(0,DateTimeOffset.UtcNow,CaptureState.Failed,"Prepare",e.HResult));throw; }
    }
    private void CaptureEvent(CaptureObservation e) { lock(gate) { summary.Capture.Add(e); if(summary.Capture.Count>32)summary.Capture.RemoveAt(0); Save(); } }
    public string CaptureDirectory => Path.Combine(directory, "capture");
    private static readonly SemaphoreSlim QuerySlots = new(2);
    public Guid Id => summary.RunId;
    public string NativeDataDirectory { get; }
    public bool HasObservedProcess { get { lock (gate) return tracked.Count != 0; } }
    private sealed class TrackedProcess(SafeProcessHandle handle, ProcessRunSummary report)
    {
        public SafeProcessHandle Handle { get; } = handle;
        public ProcessRunSummary Report { get; } = report;
        public bool QueryRunning;
        public DateTimeOffset NextQuery;
    }
    public GameRunDiagnostics(string executablePath, string dataRoot, ICrashEventSource? eventSource = null, bool startWorker = true, RunLaunchProfile? profile = null)
    {
        executable = Path.GetFullPath(executablePath); source = eventSource ?? new WindowsCrashEventSource();
        summary = new() { RunId = Guid.NewGuid(), StartedUtc = DateTimeOffset.UtcNow, Outcome = RunOutcome.Starting,
            ExecutablePathSha256 = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(executable.ToUpperInvariant()))) };
        string root = Path.Combine(Path.GetFullPath(dataRoot), "diagnostics-v2");
        if (!DiagnosticSafety.Plain(root)) throw new IOException("Diagnostic directory is a reparse point.");
        directory = Path.Combine(root, Id.ToString("N")); Directory.CreateDirectory(directory);
        NativeDataDirectory = Path.Combine(directory, "native"); Directory.CreateDirectory(NativeDataDirectory);
        summary.LaunchProfile = profile ?? new(RunGameMode.Unknown);
        summary.GameplayRequested = summary.LaunchProfile.Mode == RunGameMode.MOD;
        logs = new(NativeDataDirectory, Stage, StageDetail);
        InspectComponents(); Stage(RunStage.Prepared); Save();
        RunDiagnosticStore.Trim(root, Id);
        NativeCaptureSession.TrimPrivate(root);
        if (startWorker) _ = Task.Run(Worker);
    }
    public bool Observe(Process process, ProcessEvidence evidence)
    {
        SafeProcessHandle? owned = null;
        try {
            if (!DuplicateHandle(GetCurrentProcess(), process.SafeHandle, GetCurrentProcess(), out owned, 0, false, 2)) return ObservationFailed();
            var path = new StringBuilder(32768); int size = path.Capacity;
            if (!QueryFullProcessImageName(owned, 0, path, ref size) || !path.ToString().Equals(executable, StringComparison.OrdinalIgnoreCase) ||
                !GetProcessTimes(owned, out long created, out _, out _, out _)) return ObservationFailed();
            var started = new DateTimeOffset(DateTime.FromFileTimeUtc(created));
            // The PID alone and a same-name executable are never sufficient evidence.
            if (started < summary.StartedUtc.AddSeconds(-1)) return false;
            lock (gate) {
                if (stopped || closeRequested || tracked.Count >= 16) return false;
                if (summary.SessionClosed && (evidence != ProcessEvidence.ValidatedConfigurationPeer || summary.Intent != RunIntent.None
                    || TerminalExit(summary) || summary.Capture.Any(c => c.State == CaptureState.Captured))) return false;
                var prior = tracked.FirstOrDefault(t => t.Report.Pid == process.Id && t.Report.CreatedUtc == started);
                if (prior != null) {
                    if (evidence == ProcessEvidence.ValidatedConfigurationPeer) prior.Report.Evidence = evidence;
                    Stage(RunStage.ConfigurationPeer, process.Id);
                } else {
                    // A late validated retail handoff reopens only the automatic zero-exit observation window.
                    // Explicit close, user intent, terminal fault and stopped workers never revive.
                    if (summary.SessionClosed) { summary.SessionClosed = false; closedUtc = default; }
                    var report = new ProcessRunSummary { Pid = process.Id, CreatedUtc = started, Evidence = evidence };
                    foreach (var previous in tracked.Where(t => t.Report.CreatedUtc <= started)) previous.Report.SuccessorObserved = true;
                    tracked.Add(new(owned, report)); owned = null; summary.Processes.Add(report);
                    Stage(evidence == ProcessEvidence.CreatedByLauncher ? RunStage.ProcessCreated : RunStage.ConfigurationPeer, process.Id); Save();
                }
            }
            capture?.Observe(process);
            return true;
        } catch (Exception) { return ObservationFailed(); }
        finally { owned?.Dispose(); }
    }
    private bool ObservationFailed() { Stage(RunStage.ObservationUnavailable); return false; }
    public void Stage(RunStage stage, int? pid = null)
    {
        lock (gate) {
            if (stopped) return;
            summary.Stages.Add(new(DateTimeOffset.UtcNow, stage, pid)); if (summary.Stages.Count > 256) summary.Stages.RemoveAt(0);
            if (stage == RunStage.DuplicateFusionModules) summary.DuplicateFusionModulesObserved = true;
        }
    }
    private void StageDetail(RunEvent e) { lock(gate) { summary.Stages.Add(e); if(summary.Stages.Count>256)summary.Stages.RemoveAt(0); if(e.Stage==RunStage.DuplicateFusionModules)summary.DuplicateFusionModulesObserved=true; } }
    public void LauncherEvent(DiagnosticEvents.Entry e)
    {
        lock (gate) {
            if (stopped || summary.SessionClosed) return;
            summary.LauncherEvents.Add(DiagnosticSafety.Clean(new LauncherRunEvent(e.Utc, e.Level, e.Code, e.ExceptionType, e.HResult, e.SocketError)));
            if (summary.LauncherEvents.Count > 200) summary.LauncherEvents.RemoveAt(0);
        }
    }
    public void MarkStable()
    {
        lock (gate) { if (tracked.Any(t => !t.Report.ExitObserved && !t.Report.ObservationUnavailable)) { summary.StableObserved = true; Stage(RunStage.Stable); Save(); } }
    }
    public void SetIntent(RunIntent intent)
    {
        lock (gate) { if (intent != RunIntent.None && summary.Intent == RunIntent.None) { summary.Intent = intent; summary.IntentUtc = DateTimeOffset.UtcNow; Stage(RunStage.UserAction); Save(); } }
    }
    public void Close()
    {
        lock (gate) closeRequested = true;
        Tick(); capture?.Dispose();
        lock (gate) { if (!summary.SessionClosed) { summary.SessionClosed = true; closedUtc = DateTimeOffset.UtcNow; Save(); } }
    }
    public RunSummary Snapshot()
    {
        lock (gate) { UpdateOutcome(); return JsonSerializer.Deserialize<RunSummary>(JsonSerializer.Serialize(summary, DiagnosticSafety.Json), DiagnosticSafety.Json)!; }
    }
    public void Flush() { Tick();lock(gate)Save(); }
    public void Tick() { lock(tickGate)TickCore(); }
    private void TickCore()
    {
        List<TrackedProcess> queries = [];
        lock (gate) {
            if (stopped) return;
            foreach (var t in tracked) {
                if (!t.Report.ExitObserved && !t.Report.ObservationUnavailable) {
                    uint state = WaitForSingleObject(t.Handle, 0);
                    if (state == 0) {
                        t.Report.ExitObserved = true;
                        t.Report.ExitUtc = GetProcessTimes(t.Handle, out _, out long exit, out _, out _) && exit > 0 ? new DateTimeOffset(DateTime.FromFileTimeUtc(exit)) : DateTimeOffset.UtcNow;
                        if (GetExitCodeProcess(t.Handle, out uint code)) t.Report.ExitCodeDecimal = code;
                        else t.Report.ObservationUnavailable = true;
                        t.Report.WerState = CrashQueryState.Pending; t.NextQuery = DateTimeOffset.UtcNow;
                        Stage(RunStage.ProcessExited, t.Report.Pid);
                    } else if (state == uint.MaxValue) { t.Report.ObservationUnavailable = true; Stage(RunStage.ObservationUnavailable, t.Report.Pid); }
                }
                if (t.Report.ExitObserved && !t.QueryRunning && t.Report.WerAttempts < 5 && DateTimeOffset.UtcNow >= t.NextQuery) {
                    t.QueryRunning = true; t.Report.WerAttempts++; queries.Add(t);
                }
            }
        }
        logs.Poll(); capture?.Poll();
        if(ticks % 4 == 0) InspectLoadedModules();
        lock(gate) {
            if(!summary.SessionClosed && summary.Processes.Count>0 && summary.Processes.All(p=>p.ExitObserved || p.ObservationUnavailable)) {
                var last=summary.Processes.OrderBy(p=>p.CreatedUtc).Last();
                if(TerminalExit(summary) || last.ExitUtc is {} end && DateTimeOffset.UtcNow-end>TimeSpan.FromSeconds(30)) {
                    summary.SessionClosed=true;closedUtc=DateTimeOffset.UtcNow;
                }
            }
        }
        foreach (var t in queries) _ = Task.Run(() => ReadCrash(t));
        lock (gate) { if (++ticks % 4 == 0 || queries.Count != 0) Save(); }
    }
    private async Task ReadCrash(TrackedProcess t)
    {
        bool slot = false;
        try {
            if (!await QuerySlots.WaitAsync(TimeSpan.FromSeconds(1)).ConfigureAwait(false)) {
                lock(gate) { t.Report.WerState=CrashQueryState.TimedOut;t.QueryRunning=false;t.NextQuery=DateTimeOffset.UtcNow.AddSeconds(10);Save(); }
                return;
            }
            slot = true;
            var context = new CrashQueryContext(executable, t.Report.Pid, t.Report.CreatedUtc, t.Report.ExitUtc!.Value);
            var work = Task.Run(() => source.Read(context));
            if (await Task.WhenAny(work, Task.Delay(TimeSpan.FromSeconds(4))).ConfigureAwait(false) != work) {
                lock(gate) { t.Report.WerState=CrashQueryState.TimedOut;t.Report.WerAttempts=5;Save(); }
                // Keep the slot occupied until the native call really returns. Never spawn replacement reads indefinitely.
                slot=false;_ = work.ContinueWith(_ => QuerySlots.Release(), TaskScheduler.Default);return;
            }
            var read = await work.ConfigureAwait(false);
            lock (gate) {
                t.Report.Faults = CrashEventCorrelation.Match(context, read.Xml, t.Report.Faults);
                t.Report.WerState = t.Report.Faults.Count > 0 ? CrashQueryState.Matched :
                    read.State == CrashQueryState.NoCorrelatedEventYet && t.Report.WerAttempts >= 5 ? CrashQueryState.NoCorrelatedEventWithinWindow : read.State;
                t.Report.WerWin32Error = read.Win32Error;
                t.NextQuery = DateTimeOffset.UtcNow.AddSeconds(new[] { 0, 1, 3, 10, 30, 30 }[Math.Min(t.Report.WerAttempts, 5)]);
                t.QueryRunning = false; Save();
            }
        } catch { lock (gate) { t.Report.WerState = CrashQueryState.Unavailable; t.QueryRunning = false; t.NextQuery = DateTimeOffset.UtcNow.AddSeconds(10); Save(); } }
        finally { if(slot)QuerySlots.Release(); }
    }
    private async Task Worker()
    {
        try {
            while (true) {
                Tick();
                lock (gate) {
                    // Preserve handles through startup handoffs and after UI cleanup; never wait indefinitely after the session closes.
                    if (summary.SessionClosed && DateTimeOffset.UtcNow - closedUtc > TimeSpan.FromSeconds(75)) { stopped = true; Save(); break; }
                }
                await Task.Delay(250).ConfigureAwait(false);
            }
        } catch { lock (gate) { summary.Outcome = RunOutcome.ObservationUnavailable; Save(); } }
        finally { capture?.Dispose(); logs.Dispose(); TrimClosedRawLogs(); lock (gate) { foreach (var t in tracked) t.Handle.Dispose(); } RunDiagnosticStore.Trim(Path.GetDirectoryName(directory)!, Id); }
    }
    private void UpdateOutcome()
    {
        summary.UpdatedUtc = DateTimeOffset.UtcNow;
        if (summary.Processes.Any(p => !p.ExitObserved && !p.ObservationUnavailable)) { summary.Outcome = summary.StableObserved ? RunOutcome.Running : RunOutcome.Starting; return; }
        var last = summary.Processes.OrderBy(p => p.CreatedUtc).LastOrDefault();
        if(summary.Processes.Any(p => p.Faults.Count>0) || summary.Capture.Any(c=>c.State==CaptureState.Captured)) { summary.Outcome=RunOutcome.ConfirmedCrash;return; }
        // A later retry is an additional user action, never an explanation for an earlier abnormal exit.
        if(last?.ExitObserved==true && last.ExitCodeDecimal is > 0 && (summary.IntentUtc==null || last.ExitUtc<summary.IntentUtc)) {summary.Outcome=RunOutcome.NonzeroExit;return;}
        if (summary.Intent != RunIntent.None) { summary.Outcome = Enum.Parse<RunOutcome>(summary.Intent.ToString()); return; }
        if (last == null) { summary.Outcome = summary.SessionClosed ? RunOutcome.ObservationUnavailable : RunOutcome.Starting; return; }
        if (last.ObservationUnavailable || !last.ExitObserved) { summary.Outcome = RunOutcome.ObservationUnavailable; return; }
        if (!summary.StableObserved) { summary.Outcome = DateTimeOffset.UtcNow - last.ExitUtc < TimeSpan.FromSeconds(30) ? RunOutcome.AwaitingHandoff : RunOutcome.EarlyExitUnconfirmed; return; }
        summary.Outcome = last.ExitCodeDecimal == 0 ? RunOutcome.NormalExit : RunOutcome.NonzeroExit;
    }
    private void Save()
    {
        try {
            UpdateOutcome();
            string path = Path.Combine(directory, "run.json");
            if (DiagnosticSafety.Plain(path)) ScblLan.LocalData.AtomicWrite(path, JsonSerializer.Serialize(summary, DiagnosticSafety.Json));
        } catch { /* Diagnostics must not change launch behavior or prevent cleanup. */ }
    }
    private void InspectComponents()
    {
        string dir = Path.GetDirectoryName(executable)!;
        foreach (string name in DiagnosticSafety.Modules) {
            string path = name == "Blacklist_DX11_game.exe" ? executable : name == "scbl_gameplay.dll" ? Path.Combine(RuntimePaths.Root, name) :
                name.EndsWith(".asi", StringComparison.OrdinalIgnoreCase) ? Path.Combine(dir, "scripts", name) : Path.Combine(dir, name);
            var value = RunDiagnosticStore.Fingerprint(path, name); summary.Components.Add(value);
            if (name == "Blacklist_DX11_game.exe") summary.ExecutableSha256 = value.Sha256;
        }
        summary.DuplicateFusionFiles = new[] { "SplinterCellBlacklist.FusionFix.asi", "SplinterCellBlacklist.FusionMod.asi" }
            .All(n => summary.Components.Any(c => c.Name == n && c.Status == FingerprintState.Read));
    }
    private void InspectLoadedModules()
    {
        TrackedProcess[] live; lock(gate)live=tracked.Where(t=>!t.Report.ExitObserved && !t.Report.ObservationUnavailable).ToArray();
        foreach(var t in live) try {
            if(WaitForSingleObject(t.Handle,0)!=258)continue;
            using var p=Process.GetProcessById(t.Report.Pid);
            if(p.StartTime.ToUniversalTime()!=t.Report.CreatedUtc.UtcDateTime)continue;
            foreach(ProcessModule m in p.Modules) {
                if(moduleHashes.Count>=256) {lock(gate)summary.LoadedModulesState="Partial";break;}
                string path=m.FileName; if(!moduleHashes.TryGetValue(path,out var f)) { f=RunDiagnosticStore.Fingerprint(path,DiagnosticSafety.Module(m.ModuleName)); moduleHashes[path]=f; }
                string game=Path.GetDirectoryName(executable)!;
                bool Under(string root)=>Path.GetFullPath(path).StartsWith(Path.GetFullPath(root).TrimEnd(Path.DirectorySeparatorChar)+Path.DirectorySeparatorChar,StringComparison.OrdinalIgnoreCase);
                var source=Under(Path.Combine(game,"scripts"))?ModuleSource.GameScripts:Under(game)?ModuleSource.GameDirectory:Under(RuntimePaths.Root)?ModuleSource.PackagedRuntime:Under(Environment.GetFolderPath(Environment.SpecialFolder.Windows))?ModuleSource.Windows:ModuleSource.Other;
                lock(gate) {
                    summary.LoadedModulesState="Observed";
                    if(!summary.LoadedModules.Any(x=>x.Pid==p.Id && x.CreatedUtc==t.Report.CreatedUtc && x.Name==f.Name && x.Sha256==f.Sha256)) {
                        summary.LoadedModules.Add(new(p.Id,t.Report.CreatedUtc,DateTimeOffset.UtcNow,f.Name,source,f.Status,f.Sha256));
                        if(summary.LoadedModules.Count>512)summary.LoadedModules.RemoveAt(0);
                    }
                }
            }
        } catch {lock(gate)summary.LoadedModulesState=summary.LoadedModules.Count>0?"Partial":"Unavailable";}
    }
    private void TrimClosedRawLogs()
    {
        // Native writer owns its active file. Only after every retained identity has exited may
        // we keep a bounded tail. Strictly limited to this generated UUID, never legacy logs.
        lock(gate)if(tracked.Any(t=>!t.Report.ExitObserved))return;
        foreach(var name in new[]{"bl-tracing.log","hooks-party-trace.log"}) try {
            string p=Path.Combine(NativeDataDirectory,"logs","game",name);
            if(!DiagnosticSafety.Plain(p)||!File.Exists(p)||new FileInfo(p).Length<=8L*1024*1024)continue;
            using var f=new FileStream(p,FileMode.Open,FileAccess.ReadWrite,FileShare.None);
            byte[] tail=new byte[256*1024];f.Position=f.Length-tail.Length;f.ReadExactly(tail);f.Position=0;f.Write(tail);f.SetLength(tail.Length);
        } catch { }
    }
    [DllImport("kernel32.dll")] private static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool DuplicateHandle(IntPtr sourceProcess, SafeProcessHandle source, IntPtr targetProcess, out SafeProcessHandle target, uint access, bool inherit, uint options);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern bool QueryFullProcessImageName(SafeProcessHandle process, int flags, StringBuilder name, ref int size);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool GetProcessTimes(SafeProcessHandle process, out long created, out long exit, out long kernel, out long user);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool GetExitCodeProcess(SafeProcessHandle process, out uint code);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern uint WaitForSingleObject(SafeProcessHandle handle, uint milliseconds);
}

public static class RunDiagnosticStore
{
    private static readonly object Gate = new();
    private static GameRunDiagnostics? current;
    public static GameRunDiagnostics? Current { get { lock (Gate) return current; } }
    public static GameRunDiagnostics? Begin(string executable, string? dataRoot = null, RunLaunchProfile? profile = null)
    {
        try { lock (Gate) { current = new(executable, dataRoot ?? LogService.AppDataDir, profile: profile); return current; } }
        catch(Exception e) { DiagnosticEvents.RecordException(e);DiagnosticEvents.Record("ERROR", "game diagnostic observation unavailable"); return null; }
    }
    public static void Record(DiagnosticEvents.Entry e) { try { Current?.LauncherEvent(e); } catch { } }
    public static async Task PrepareExportAsync()
    {
        try {
            var run=Current;if(run==null)return;
            // A single click gives late WER a short opportunity; never blocks indefinitely or reads arbitrary old sessions.
            for(int i=0;i<20;i++) {
                run.Flush();
                if(!run.Snapshot().Processes.Any(p=>p.ExitObserved && p.WerState is CrashQueryState.Pending or CrashQueryState.NoCorrelatedEventYet))return;
                await Task.Delay(250).ConfigureAwait(false);
            }
            run.Flush();
        } catch { }
    }
    public static object Export(string? dataRoot = null)
    {
        string root = Path.Combine(dataRoot ?? LogService.AppDataDir, "diagnostics-v2"); var runs = new List<RunSummary>(); int rejected = 0;
        try {
            if (!Directory.Exists(root)) return new { Schema = 2, Status = "NoRecordedRuns", Runs = runs } as object;
            if (!DiagnosticSafety.Plain(root)) return new { Schema = 2, Status = "ReparsePointSkipped", Runs = runs };
            foreach (string dir in Directory.EnumerateDirectories(root).Where(p => Guid.TryParseExact(Path.GetFileName(p), "N", out _)).OrderByDescending(Directory.GetLastWriteTimeUtc).Take(4)) {
                string file = Path.Combine(dir, "run.json");
                try {
                    if (!DiagnosticSafety.Plain(file) || !File.Exists(file) || new FileInfo(file).Length > 512 * 1024) { rejected++; continue; }
                    var value = JsonSerializer.Deserialize<RunSummary>(File.ReadAllText(file), DiagnosticSafety.Json);
                    var clean = value == null ? null : DiagnosticSafety.Clean(value);
                    if (clean == null || clean.RunId.ToString("N") != Path.GetFileName(dir)) { rejected++; continue; }
                    runs.Add(clean);
                } catch { rejected++; }
            }
            return new { Schema = 2, Status = "Read", PersistedSnapshotsNotLiveProcessClaims = true, RejectedRecords = rejected, Runs = runs,
                Limits = "4 runs; 16 processes/run; 256 native stage events; 200 launcher events; no raw log/dump. Native log polling may miss lines overwritten inside a startup handoff. WER missing is not evidence of a normal exit. Duplicate Fusion files/modules are observations, not a crash diagnosis." };
        } catch { return new { Schema = 2, Status = "Unavailable", Runs = runs }; }
    }
    internal static ModuleFingerprint Fingerprint(string path, string name)
    {
        try {
            if (!File.Exists(path)) return new(name, FingerprintState.Missing, null, null, null);
            if (!DiagnosticSafety.Plain(path)) return new(name, FingerprintState.ReparsePointSkipped, null, null, null);
            using var f = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
            if (f.Length > 256L * 1024 * 1024) return new(name, FingerprintState.TooLarge, f.Length, null, null);
            return new(name, FingerprintState.Read, f.Length, Convert.ToHexString(SHA256.HashData(f)), DiagnosticSafety.Version(FileVersionInfo.GetVersionInfo(path).FileVersion));
        } catch { return new(name, FingerprintState.Unavailable, null, null, null); }
    }
    private static bool TryRetireRunFiles(string metadata,string snapshot)
    {
        try {
            bool hasMetadata=File.Exists(metadata),hasSnapshot=File.Exists(snapshot);
            if(hasSnapshot&&!hasMetadata)return false;
            foreach(string file in new[]{metadata,snapshot}) {
                if(!DiagnosticSafety.Plain(file)||Directory.Exists(file))return false;
                if(File.Exists(file)&&(File.GetAttributes(file)&FileAttributes.ReadOnly)!=0)return false;
            }
            // DELETE access rejects any existing handle that denies delete sharing. Keeping these
            // handles also prevents a new non-delete-sharing opener between preflight and removal.
            using SafeFileHandle? meta=hasMetadata?OpenForRetentionDelete(metadata):null;
            if(meta?.IsInvalid==true)return false;
            using SafeFileHandle? config=hasSnapshot?OpenForRetentionDelete(snapshot):null;
            if(config?.IsInvalid==true)return false;
            if(!DiagnosticSafety.Plain(metadata)||!DiagnosticSafety.Plain(snapshot))return false;
            if(config!=null&&!RetentionDisposition(config,true))return false;
            if(meta!=null&&!RetentionDisposition(meta,true)) {
                // Nothing has closed yet. Best effort withdraw the first pending deletion.
                if(config!=null)RetentionDisposition(config,false);
                return false;
            }
            return true; // Disposing the exact handles completes deletion, never a replaced path.
        } catch(Exception e) when(e is IOException or UnauthorizedAccessException){return false;}
    }
    private static SafeFileHandle OpenForRetentionDelete(string path)
    {
        // Match .NET file IO's long-path support for deeply nested diagnostic roots.
        string full=Path.GetFullPath(path);
        string native=full.StartsWith(@"\\?\",StringComparison.Ordinal)?full:full.StartsWith(@"\\",StringComparison.Ordinal)?@"\\?\UNC\"+full[2..]:@"\\?\"+full;
        return CreateRetentionHandle(native,0x00010000,7,IntPtr.Zero,3,0x80,IntPtr.Zero);
    }
    private static bool RetentionDisposition(SafeFileHandle handle,bool delete) {int disposition=delete?1:0;return SetRetentionDisposition(handle,4,ref disposition,sizeof(int));}
    [DllImport("kernel32.dll",EntryPoint="CreateFileW",CharSet=CharSet.Unicode,SetLastError=true)]
    private static extern SafeFileHandle CreateRetentionHandle(string path,uint access,uint share,IntPtr security,uint creation,uint flags,IntPtr template);
    [DllImport("kernel32.dll",EntryPoint="SetFileInformationByHandle",SetLastError=true)]
    [return:MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetRetentionDisposition(SafeFileHandle handle,int infoClass,ref int info,uint size);
    internal static void Trim(string root, Guid keep)
    {
        // Only exact generated UUID directories and four known files. Never recurse or touch other data.
        try {
            if (!DiagnosticSafety.Plain(root)) return;
            var dirs = Directory.EnumerateDirectories(root).Where(p => Guid.TryParseExact(Path.GetFileName(p), "N", out _) && Path.GetFileName(p) != keep.ToString("N"))
                .OrderByDescending(Directory.GetLastWriteTimeUtc).Skip(3).Take(64);
            foreach (string dir in dirs) {
                if (!DiagnosticSafety.Plain(dir)) continue;
                string metadata = Path.Combine(dir,"run.json");
                if(!DiagnosticSafety.Plain(metadata))continue;
                if(File.Exists(metadata) && new FileInfo(metadata).Length<=512*1024) {
                    var prior=JsonSerializer.Deserialize<RunSummary>(File.ReadAllText(metadata),DiagnosticSafety.Json);
                    // Do not prune another currently active launcher's run. Closed history is always count-bounded.
                    if(prior?.SessionClosed!=true && DateTime.UtcNow-Directory.GetLastWriteTimeUtc(dir)<TimeSpan.FromMinutes(2))continue;
                }
                // Obtain delete access to BOTH files before marking either for deletion.
                // A locked/readonly metadata or snapshot defers the pair, preserving ownership.
                if(!TryRetireRunFiles(metadata,Path.Combine(dir,RequestedConfigurationSnapshot.FileName)))continue;
                foreach (string rel in new[] { "native/logs/game/bl-tracing.log", "native/logs/game/hooks-party-trace.log" }) {
                    string file = Path.GetFullPath(Path.Combine(dir, rel));
                    if (file.StartsWith(Path.GetFullPath(root) + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase) && DiagnosticSafety.Plain(file) && File.Exists(file)) File.Delete(file);
                }
                foreach (string rel in new[] { "native/logs/game", "native/logs", "native", "" }) {
                    string d = Path.Combine(dir, rel); if (Directory.Exists(d) && !Directory.EnumerateFileSystemEntries(d).Any()) Directory.Delete(d, false);
                }
            }
        } catch { }
    }
}

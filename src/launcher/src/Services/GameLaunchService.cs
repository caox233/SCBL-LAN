using System.Diagnostics;
using ScblLan;
namespace SplinterCellCNLauncher.Services;
public sealed class GameLaunchService
{
    private LaunchSession? session;
    private OwnedUnconfirmedLaunch? unconfirmedLaunch;
    private readonly object unconfirmedGate = new();
    private readonly object createdProcessFailureKey = new();
    private Guid? completedUnconfirmedSession;
    internal bool CaptureNextLaunch { get; set; }
    public bool HasConfirmedEarlyExit(Guid? id) => session?.Id==id && session?.Diagnostics?.HasTerminalExit==true;
    public Guid? SessionId => session?.Id;
    internal string? DiagnosticDataRoot { get; set; }
    public void SetDiagnosticIntent(RunIntent intent) => session?.Diagnostics?.SetIntent(intent);
    public void MarkDiagnosticStable() => session?.Diagnostics?.MarkStable();

    public async Task<Process> StartGameAsync(string gameDir,string gameExecutable,LaunchPipe pipe,CancellationToken cancellationToken=default,string? pluginDirectory=null)
    {
        if (session != null) throw new IOException("上一次游戏配置会话尚未结束。");
        var prior = UnconfirmedLaunch;
        if (prior != null) ReleaseExitedUnconfirmedLaunch(prior.SessionId);
        if (UnconfirmedLaunch != null) throw new IOException("上一次启动尚未确认，但本次创建的进程仍在运行；请先结束该进程。");
        string path=GameRuntime.ValidateExecutable(Path.Combine(gameDir,gameExecutable));
        bool captureOnce=CaptureNextLaunch;CaptureNextLaunch=false;
        var diagnostic=RunDiagnosticStore.Begin(path,DiagnosticDataRoot,new(pluginDirectory==null?RunGameMode.Official:RunGameMode.MOD,RequestedSource:pluginDirectory==null?ModuleSource.Unknown:ModuleSource.PackagedRuntime));
        var current=new LaunchSession(pipe,Path.Combine(gameDir,"scbl-lan.toml"),diagnostic);
        session=current;
        using var timeout=CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        timeout.CancelAfter(TimeSpan.FromSeconds(90));
        Process? process=null;
        try {
            if(captureOnce)diagnostic?.EnableCapture();
            // Capture only this pipe's prepared frame for this concrete run, before its pump.
            diagnostic?.TryWriteRequestedConfiguration(pipe.RequestedConfigurationProjection);
            current.Start();
            var psi=new ProcessStartInfo(path) {WorkingDirectory=gameDir,UseShellExecute=false};
            psi.Environment["SCBL_LAN_CLIENT_DATA_DIR"]=diagnostic?.NativeDataDirectory ?? LogService.AppDataDir;
            psi.Environment.Remove("SCBL_LAN_PLUGIN_DIR");
            if(pluginDirectory!=null) psi.Environment["SCBL_LAN_PLUGIN_DIR"]=Path.GetFullPath(pluginDirectory);
            if(captureOnce) {
                // Identical image/arguments/environment/working directory; only opt-in creation
                // is suspended until the out-of-process debugger has bound the exact identity.
                using var suspended=SuspendedDiagnosticLaunch.Start(psi);
                process=suspended.Process;
                try {diagnostic?.Observe(process,ProcessEvidence.CreatedByLauncher);}
                finally {suspended.Resume();}
            } else {
                process=Process.Start(psi) ?? throw new IOException("游戏进程创建失败。");
                diagnostic?.Observe(process,ProcessEvidence.CreatedByLauncher);
            }
            await current.FirstDelivery.WaitAsync(timeout.Token);
            // The retail startup process can exit and start the real game later.
            // The UI lifetime monitor, not the first ACK, owns this session's end.
            return process;
        } catch (Exception e) {
            diagnostic?.SetIntent(e is OperationCanceledException ? RunIntent.Cancelled : RunIntent.LaunchFailed);
            if (process != null)
            {
                // This is the exact created process, not a name/path scan or a
                // temporary PeerValidated handle. Diagnostics may be unavailable.
                e.Data[createdProcessFailureKey] = current.Id;
                var captured = OwnedUnconfirmedLaunch.TryCapture(process, current.Id, path);
                lock (unconfirmedGate) unconfirmedLaunch = captured;
                if (captured == null) process.Dispose();
            }
            await StopAsync(current.Id);
            throw;
        }
    }

    public UnconfirmedLaunchInfo? UnconfirmedLaunch
    {
        get { lock (unconfirmedGate) return unconfirmedLaunch?.Info; }
    }

    public bool IsCreatedProcessFailure(Exception error) => error.Data[createdProcessFailureKey] is Guid;

    public bool ReleaseExitedUnconfirmedLaunch(Guid expectedSession)
    {
        OwnedUnconfirmedLaunch owner;
        lock (unconfirmedGate)
        {
            if (unconfirmedLaunch is not { } current || current.Info.SessionId != expectedSession || !current.HasExited) return false;
            owner = current;
            // Publish the completed identity with removal, before a queued UI
            // End can observe no live owner. This is not authority over a new run.
            completedUnconfirmedSession = expectedSession;
            unconfirmedLaunch = null;
        }
        owner.ReleaseAfterExit();
        return true;
    }

    public async Task<bool> WaitForUnconfirmedLaunchExitAsync(Guid expectedSession, CancellationToken token)
    {
        OwnedUnconfirmedLaunch owner;
        lock (unconfirmedGate)
        {
            if (unconfirmedLaunch is not { } current) return completedUnconfirmedSession == expectedSession;
            if (current.Info.SessionId != expectedSession) return false;
            owner = current;
        }
        await owner.WaitForExitAsync(token).ConfigureAwait(false);
        ReleaseExitedUnconfirmedLaunch(expectedSession);
        return owner.HasExited;
    }

    public async Task<bool> EndUnconfirmedLaunchAsync(Guid expectedSession)
    {
        OwnedUnconfirmedLaunch owner;
        lock (unconfirmedGate)
        {
            if (unconfirmedLaunch is not { } current) return completedUnconfirmedSession == expectedSession;
            if (current.Info.SessionId != expectedSession) return false;
            owner = current;
        }
        await owner.EndAsync().ConfigureAwait(false);
        ReleaseExitedUnconfirmedLaunch(expectedSession);
        return owner.HasExited;
    }

    public Task StopAsync() => StopAsync(null);
    public async Task StopAsync(Guid? expectedSession)
    {
        var current=session;
        if(current==null || expectedSession.HasValue && current.Id!=expectedSession.Value) return;
        // Retain ownership until the shared drain completes; concurrent stops must join it.
        try { await current.DisposeAsync(); }
        finally { Interlocked.CompareExchange(ref session,null,current); }
    }

    private sealed class LaunchSession(LaunchPipe pipe,string reference,GameRunDiagnostics? diagnostics) : IAsyncDisposable
    {
        public Guid Id { get; }=diagnostics?.Id ?? Guid.NewGuid();
        public GameRunDiagnostics? Diagnostics => diagnostics;
        private void ObservePeer(Process process) => diagnostics?.Observe(process,ProcessEvidence.ValidatedConfigurationPeer);
        private readonly string text=$"LaunchPipe = \"{pipe.Name}\"\n";
        private readonly CancellationTokenSource stop=new();
        private readonly TaskCompletionSource first=new(TaskCreationOptions.RunContinuationsAsynchronously);
        private Task pump=Task.CompletedTask;
        private readonly object disposeGate=new();
        private Task? disposeTask;
        private int deliveries;
        public Task FirstDelivery => first.Task;
        public void Start()
        {
            pipe.PeerValidated+=ObservePeer;
            LocalData.AtomicWrite(reference,text);
            LogService.Info("LAN configuration session opened; retained across game startup handoffs.");
            pump=ServeAsync();
        }
        private async Task ServeAsync()
        {
            try {
                while(!stop.IsCancellationRequested) {
                    try {await pipe.DeliverAsync(stop.Token);diagnostics?.Stage(RunStage.ConfigurationDelivered);LogService.Info($"LAN configuration delivery #{++deliveries} acknowledged; session remains active.");first.TrySetResult();}
                    catch(OperationCanceledException) when(stop.IsCancellationRequested) {break;}
                    catch(ObjectDisposedException) {break;}
                    catch(Exception e) when(e is IOException or ArgumentException or InvalidOperationException or OperationCanceledException) {
                        // Reject this peer without exposing credentials or disabling the next game stage.
                        LogService.Error("LAN configuration peer rejected: "+e.GetType().Name);
                    }
                }
            } catch(Exception e) {first.TrySetException(e);LogService.Error("LAN configuration session ended: "+e.GetType().Name);}
            finally {first.TrySetCanceled();}
        }
        public ValueTask DisposeAsync()
        {
            lock(disposeGate) return new ValueTask(disposeTask ??= DisposeCoreAsync());
        }
        private async Task DisposeCoreAsync()
        {
            diagnostics?.Close();
            pipe.PeerValidated-=ObservePeer;
            stop.Cancel();
            // Only remove this session's non-secret reference, never another writer's file.
            try {if(File.Exists(reference) && File.ReadAllText(reference)==text) File.Delete(reference);}
            catch(Exception e) when(e is IOException or UnauthorizedAccessException) {LogService.Error("LAN configuration reference cleanup: "+e.GetType().Name);}
            try {await pump.ConfigureAwait(false);}
            finally {await pipe.DisposeAsync();stop.Dispose();LogService.Info("LAN configuration session closed; in-memory frame cleared.");}
        }
    }
}

using System.ComponentModel;
using System.Diagnostics;

namespace SplinterCellCNLauncher.Services;

public sealed record UnconfirmedLaunchInfo(Guid SessionId, int ProcessId, DateTime CreatedUtc, string ExecutablePath, bool HasExited);

// Owns only the exact Process returned by this launch's Process.Start. Neither a
// matching name/directory nor optional diagnostics can manufacture this owner.
internal sealed class OwnedUnconfirmedLaunch
{
    private readonly Process process;
    private readonly Task exited;
    private readonly object gate = new();
    private readonly Guid sessionId;
    private readonly int processId;
    private readonly DateTime createdUtc;
    private readonly string executable;
    private Task? endTask;
    private bool released;

    private OwnedUnconfirmedLaunch(Process process, Guid sessionId, string executable)
    {
        this.process = process;
        this.sessionId = sessionId;
        this.executable = Path.GetFullPath(executable);
        processId = process.Id;
        createdUtc = process.StartTime.ToUniversalTime();
        if (process.HasExited || !SameIdentity())
            throw new InvalidOperationException("The created launch identity is not live and confirmed.");
        _ = process.SafeHandle; // retain the created OS identity, not a future PID lookup
        exited = process.WaitForExitAsync();
    }

    internal static OwnedUnconfirmedLaunch? TryCapture(Process process, Guid sessionId, string executable)
    {
        try { return new OwnedUnconfirmedLaunch(process, sessionId, executable); }
        catch (Exception ex) when (ex is InvalidOperationException or Win32Exception or IOException or UnauthorizedAccessException)
        {
            return null; // Unknown/handed-off processes are never inferred by name.
        }
    }

    private bool SameIdentity() => process.Id == processId
        && process.StartTime.ToUniversalTime() == createdUtc
        && string.Equals(process.MainModule?.FileName, executable, StringComparison.OrdinalIgnoreCase);

    internal bool HasExited => exited.IsCompletedSuccessfully;
    internal UnconfirmedLaunchInfo Info => new(sessionId, processId, createdUtc, executable, HasExited);

    // All observers share one real exit observation. Cancelling a UI monitor
    // cancels only that wait, not ownership or the underlying observation.
    internal Task WaitForExitAsync(CancellationToken token) => exited.WaitAsync(token);

    internal Task EndAsync()
    {
        lock (gate)
        {
            if (released) return Task.CompletedTask; // release requires observed exit
            if (endTask == null || endTask.IsFaulted || endTask.IsCanceled)
                endTask = EndCoreAsync();
            return endTask;
        }
    }

    private async Task EndCoreAsync()
    {
        if (!HasExited)
        {
            try
            {
                if (!process.HasExited)
                {
                    if (!SameIdentity()) throw new IOException("无法确认本次进程身份，请手动关闭后重试。");
                    process.Kill(entireProcessTree: false);
                }
            }
            catch (InvalidOperationException) when (HasExited) { }
        }
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        await exited.WaitAsync(timeout.Token).ConfigureAwait(false);
    }

    internal bool ReleaseAfterExit()
    {
        lock (gate)
        {
            if (!HasExited) return false;
            if (!released)
            {
                released = true;
                // Waiters touch only the completed shared Task. EndCore performs
                // no further Process access after the shared exit completes.
                process.Dispose();
            }
            return true;
        }
    }
}

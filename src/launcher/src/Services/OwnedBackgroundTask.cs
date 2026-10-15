namespace SplinterCellCNLauncher.Services;

/// <summary>Owns one replaceable asynchronous loop. Restart drains its predecessor
/// without blocking the caller/UI; cancellation sources live until their loop exits.</summary>
internal sealed class OwnedBackgroundTask(Action<Exception> reportError)
{
    private readonly object gate = new();
    private Run? current;
    private Task tail = Task.CompletedTask;

    private sealed class Run
    {
        private readonly object lifetime = new();
        private CancellationTokenSource? source = new();
        public CancellationToken Token { get; }
        public Run() => Token = source!.Token;

        public void Cancel()
        {
            lock (lifetime) source?.Cancel();
        }

        public void Dispose()
        {
            lock (lifetime) { source?.Dispose(); source = null; }
        }
    }

    private void Report(Exception error)
    {
        // Reporting must not strand the next generation or create unobserved tasks.
        try { reportError(error); } catch { }
    }

    private void CancelCurrent()
    {
        try { current?.Cancel(); } catch (Exception error) { Report(error); }
    }

    public void Start(Func<CancellationToken, Task> body)
    {
        ArgumentNullException.ThrowIfNull(body);
        lock (gate)
        {
            CancelCurrent();
            var predecessor = tail;
            var run = new Run();
            current = run;
            tail = Task.Run(async () =>
            {
                try
                {
                    await predecessor.ConfigureAwait(false);
                    run.Token.ThrowIfCancellationRequested();
                    await body(run.Token).ConfigureAwait(false);
                }
                catch (OperationCanceledException) when (run.Token.IsCancellationRequested) { }
                catch (Exception error) { Report(error); }
                finally { run.Dispose(); }
            });
        }
    }

    public void Cancel()
    {
        lock (gate) CancelCurrent();
    }

    public Task StopAsync()
    {
        lock (gate) { CancelCurrent(); return tail; }
    }
}

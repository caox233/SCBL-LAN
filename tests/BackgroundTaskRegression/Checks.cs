using System.Collections.Concurrent;
using System.Net;
using System.Net.Sockets;
using SplinterCellCNLauncher.Services;

internal static class BackgroundTaskChecks
{
    static TaskCompletionSource Gate() => new(TaskCreationOptions.RunContinuationsAsynchronously);
    static async Task Wait(Task task) => await task.WaitAsync(TimeSpan.FromSeconds(5));
    public static async Task Run(Action<bool, string> check)
    {
        void Check(bool pass, string name)
        {
            check(pass, name);
            if (!pass) throw new Exception(name);
        }
        var errors = new ConcurrentQueue<Exception>();
        var owner = new OwnedBackgroundTask(errors.Enqueue);
        var entered = Gate(); var release = Gate(); var replaced = Gate();
        int active = 0, peak = 0;
        owner.Start(async token =>
        {
            peak = Interlocked.Increment(ref active);
            entered.SetResult();
            await release.Task; // Simulate a non-cancellable operation already in flight.
            Check(token.IsCancellationRequested, "replacement cancels preceding generation");
            Interlocked.Decrement(ref active);
        });
        await Wait(entered.Task);
        owner.Start(token =>
        {
            peak = Math.Max(peak, Interlocked.Increment(ref active));
            Interlocked.Decrement(ref active);
            replaced.SetResult();
            return Task.CompletedTask;
        });
        await Task.Delay(75);
        Check(!replaced.Task.IsCompleted, "replacement waits for predecessor without blocking caller");
        release.SetResult(); await Wait(replaced.Task); await Wait(owner.StopAsync());
        Check(peak == 1 && active == 0, "only one task generation owns resources");
        
        // A pending generation canceled before it starts must not acquire its resources.
        entered = Gate(); release = Gate();
        owner.Start(async _ => { entered.SetResult(); await release.Task; });
        await Wait(entered.Task);
        bool pendingRan = false;
        owner.Start(_ => { pendingRan = true; return Task.CompletedTask; });
        Task draining = owner.StopAsync();
        Check(!draining.IsCompleted, "stop exposes completion rather than only cancellation");
        release.SetResult(); await Wait(draining);
        Check(!pendingRan, "canceled queued generation never starts");
        
        var failed = Gate();
        owner.Start(_ => { failed.SetResult(); throw new InvalidOperationException("owned failure"); });
        await Wait(failed.Task); await Wait(owner.StopAsync());
        Check(errors.Count == 1, "worker failure is observed exactly once");
        var afterFailure = Gate();
        owner.Start(_ => { afterFailure.SetResult(); return Task.CompletedTask; });
        await Wait(afterFailure.Task); await Wait(owner.StopAsync());
        Check(afterFailure.Task.IsCompletedSuccessfully, "one failure does not poison later generations");
        for (int i = 0; i < 200; i++)
        {
            owner.Start(token => Task.Delay(1, token));
            owner.Cancel();
        }
        await Wait(owner.StopAsync());
        Check(errors.Count == 1, "rapid replace cancel drain does not dispose live cancellation sources");
        
        // Real loopback socket, exclusively bound; never uses the game's LAN ports.
        int port = 0;
        var receiving = Gate(); var rebound = Gate();
        owner.Start(async token =>
        {
            using var socket = new UdpClient(AddressFamily.InterNetwork);
            socket.ExclusiveAddressUse = true;
            socket.Client.Bind(new IPEndPoint(IPAddress.Loopback, 0));
            port = ((IPEndPoint)socket.Client.LocalEndPoint!).Port;
            receiving.SetResult();
            await socket.ReceiveAsync(token);
        });
        await Wait(receiving.Task);
        owner.Start(_ =>
        {
            using var socket = new UdpClient(AddressFamily.InterNetwork);
            socket.ExclusiveAddressUse = true;
            socket.Client.Bind(new IPEndPoint(IPAddress.Loopback, port));
            rebound.SetResult();
            return Task.CompletedTask;
        });
        await Wait(rebound.Task); await Wait(owner.StopAsync());
        Check(errors.Count == 1, "canceled real loopback receiver closes socket before replacement binds");
        
        var badReporter = new OwnedBackgroundTask(_ => throw new Exception("reporter failed"));
        entered = Gate();
        badReporter.Start(_ => { entered.SetResult(); throw new Exception("worker failed"); });
        await Wait(entered.Task); await Wait(badReporter.StopAsync());
        Check(true, "reporting failure cannot fault the drain chain");
    }
}

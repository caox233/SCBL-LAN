using System.Diagnostics;
using ScblLan;

namespace SplinterCellCNLauncher.Services;

// A per-attempt input snapshot. Never format credential-bearing positional fields.
internal sealed record GameLaunchRequest(
    string GameDirectory, string GameExecutable, string GameplayMode, string RuntimeRoot,
    string Username, string Password, string AccountId, string Server, string BindIp, double FovScale)
{
    public override string ToString() => nameof(GameLaunchRequest);
}

internal enum LaunchPreparationStage { Deploying, PreparingConfiguration, Starting }

internal interface ILaunchPreparationOperations
{
    string? ValidateMode(GameLaunchRequest request);
    void Deploy(GameLaunchRequest request);
    Task<LaunchPipe> PrepareAsync(GameLaunchRequest request);
    Task<Process> StartAsync(GameLaunchRequest request, LaunchPipe pipe, string? pluginDirectory);
}

// UI and tests enter this same sequence; platform/network work stays behind internal seams.
internal sealed class GameLaunchPreparation
{
    private readonly ILaunchPreparationOperations operations;
    internal GameLaunchPreparation(GameLaunchService launch) : this(new ProductionOperations(launch)) { }
    internal GameLaunchPreparation(ILaunchPreparationOperations operations) => this.operations = operations;

    internal async Task<Process> StartAsync(GameLaunchRequest request, Func<Task> beforeStart,
        Action<LaunchPreparationStage>? progress = null)
    {
        LaunchPipe? pending = null;
        try
        {
            string? pluginDirectory = operations.ValidateMode(request);
            progress?.Invoke(LaunchPreparationStage.Deploying);
            // Authentication already verified the package; install before final deployed-hash validation.
            operations.Deploy(request);
            LogService.Info("SCBL LAN Hook 已校验部署。");
            progress?.Invoke(LaunchPreparationStage.PreparingConfiguration);
            pending = await operations.PrepareAsync(request);
            LogService.Info("本次游戏认证通过受限命名管道交付，不写入旧配置文件。");
            await beforeStart();
            progress?.Invoke(LaunchPreparationStage.Starting);
            Process process = await operations.StartAsync(request, pending, pluginDirectory);
            pending = null; // Only success confirms ownership transferred to the existing session.
            return process;
        }
        finally
        {
            // Start can reject before adoption or fail after its session has already cleaned up.
            // LaunchPipe disposal is idempotent; never stop an unrelated session here.
            if (pending != null) await pending.DisposeAsync();
        }
    }

    private sealed class ProductionOperations(GameLaunchService launch) : ILaunchPreparationOperations
    {
        private readonly HookConfigService configuration = new();
        public string? ValidateMode(GameLaunchRequest request) =>
            GameplayRuntime.ValidateForLaunch(request.GameplayMode, request.RuntimeRoot);
        public void Deploy(GameLaunchRequest request) =>
            GameRuntime.Deploy(Path.Combine(request.GameDirectory, request.GameExecutable), request.RuntimeRoot);
        public Task<LaunchPipe> PrepareAsync(GameLaunchRequest request) =>
            configuration.PrepareAsync(Path.Combine(request.GameDirectory, request.GameExecutable),
                request.Username, request.Password, request.AccountId, request.Server, request.BindIp,
                request.GameplayMode, request.FovScale);
        public Task<Process> StartAsync(GameLaunchRequest request, LaunchPipe pipe, string? pluginDirectory) =>
            launch.StartGameAsync(request.GameDirectory, request.GameExecutable, pipe, pluginDirectory: pluginDirectory);
    }
}

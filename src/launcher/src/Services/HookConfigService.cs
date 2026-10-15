using ScblLan;
namespace SplinterCellCNLauncher.Services;
public sealed class HookConfigService
{
    private readonly Func<string,ReleasePackage> verifyRelease;
    public HookConfigService() : this(VerifyRelease) { }
    internal HookConfigService(Func<string,ReleasePackage> verifyRelease) => this.verifyRelease=verifyRelease;
    private static ReleasePackage VerifyRelease(string executable)
    {
        var release=ReleasePackage.Current();
        release.VerifyDeployedMain(Path.Combine(Path.GetDirectoryName(Path.GetFullPath(executable))!,"uplay_r1_loader.dll"));
        return release;
    }
    public async Task<LaunchPipe> PrepareAsync(string executable,string username,string password,string accountId,string server,string bindIp,string gameplayMode="Official",double fovScale=1.2)
    {
        if (!PasswordPolicy.IsValidForNativeGame(password))
            throw new ArgumentException("The game requires a nonempty password of at most 63 UTF-8 bytes without NUL. The stored password has not been changed.", nameof(password));
        // Both package and deployed-main verification finish in this single worker snapshot.
        var release=await Task.Run(()=>verifyRelease(executable));
        using var c=await LanConnection.ConnectAsync(server,bindIp,CancellationToken.None);
        if(c.Capabilities.ConfigPort!=80) throw new IOException("真实游戏要求配置服务端口 80；测试端口不可用于游戏。");
        GameplayRuntime.ValidateServerMode(gameplayMode,c.Capabilities.OfficialOnly);
        var authenticated=await c.Api.LoginAsync(new SplinterCellCNLauncher.Network.LoginRequest {Username=username,Password=password,ClientProduct=LanConnection.ProductContract,PackageIdentity=release.Declaration()},deadline:DateTime.UtcNow.AddSeconds(8));
        if(!string.IsNullOrEmpty(authenticated.Error) || authenticated.User?.Id!=accountId) throw new IOException("最终连接的服务器账号标识不一致，请重新连接。");
        c.AssertRouteUnchanged(bindIp);
        return new LaunchPipe(GameRuntime.Config(c,username,password,accountId,gameplayMode,fovScale,release),executable);
    }
}

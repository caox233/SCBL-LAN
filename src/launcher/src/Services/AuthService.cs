using Grpc.Core;
using ScblLan;
using SplinterCellCNLauncher.Models;
using SplinterCellCNLauncher.Network;
namespace SplinterCellCNLauncher.Services;
public sealed class AuthService
{
    private readonly Func<ReleasePackage> verifyRelease;
    public AuthService() : this(ReleasePackage.Current) { }
    internal AuthService(Func<ReleasePackage> verifyRelease) => this.verifyRelease=verifyRelease;
    public async Task<bool> CheckServerReachableAsync(string serverInput,string? localBindIp=null,CancellationToken cancellationToken=default)
    {
        try { using var c=await LanConnection.ConnectAsync(serverInput,localBindIp??"",cancellationToken); return c.Capabilities.Ready; }
        catch(OperationCanceledException) when(cancellationToken.IsCancellationRequested) { throw; }
        catch(Exception e) { LogService.Error("LAN capability check: "+e.Message);return false; }
    }
    public async Task<LoginResult> LoginAsync(string serverInput,string username,string password,string? localBindIp=null)
    {
        try {
            // Read/hash the complete package off the UI thread, before any connection.
            var release=await Task.Run(verifyRelease);
            using var c=await LanConnection.ConnectAsync(serverInput,localBindIp??"",CancellationToken.None);
            var r=await c.Api.LoginAsync(new LoginRequest { Username=username,Password=password,ClientProduct=LanConnection.ProductContract,PackageIdentity=release.Declaration() },deadline:DateTime.UtcNow.AddSeconds(8));
            return string.IsNullOrEmpty(r.Error) && !string.IsNullOrEmpty(r.User?.Id)?LoginResult.Success(r.User.Id):new LoginResult { Status=LoginStatus.ServerError,Message=r.Error };
        } catch(RpcException e) when(e.StatusCode==StatusCode.NotFound) { return new LoginResult {Status=LoginStatus.UserNotFound,Message="账号不存在。"}; }
          catch(RpcException e) when(e.StatusCode==StatusCode.Unauthenticated) { return new LoginResult {Status=LoginStatus.InvalidPassword,Message="密码错误或账号不匹配。"}; }
          catch(Exception e) { return new LoginResult {Status=LoginStatus.ConnectionFailed,Message=e.Message}; }
    }
    public async Task<RegisterResult> RegisterAsync(string serverInput,string username,string password,string ubiId,string? localBindIp=null)
    {
        try {
            // Read/hash the complete package off the UI thread, before any connection.
            var release=await Task.Run(verifyRelease);
            using var c=await LanConnection.ConnectAsync(serverInput,localBindIp??"",CancellationToken.None);
            var r=await c.Api.RegisterAsync(new RegisterRequest {Username=username,Password=password,UbiId=Guid.NewGuid().ToString(),ClientProduct=LanConnection.ProductContract,PackageIdentity=release.Declaration()},deadline:DateTime.UtcNow.AddSeconds(8));
            return string.IsNullOrEmpty(r.Error)?new RegisterResult {Status=RegisterStatus.Success,RecoveryCode=r.RecoveryCode}:new RegisterResult {Status=RegisterStatus.ServerError,Message=r.Error};
        } catch(RpcException e) when(e.StatusCode==StatusCode.AlreadyExists) { return new RegisterResult {Status=RegisterStatus.AlreadyExists,Message="账号已存在。"}; }
          catch(Exception e) { return new RegisterResult {Status=RegisterStatus.ConnectionFailed,Message=e.Message}; }
    }
    public static string BuildGrpcAddress(string input) { var s=ServerAddress.Parse(input);return $"http://{s.Host}:{s.Port}"; }
    public static string GetConfigServerHost(string input)=>ServerAddress.Parse(input).Host;
}

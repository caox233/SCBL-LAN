using Grpc.Core;
using ScblLan;
using SplinterCellCNLauncher.Network;

namespace SplinterCellCNLauncher.Services;

public sealed record PasswordRecoveryResult(bool Success, string Code, string RecoveryCode = "");

// Uses the existing server RPC, never logs or persists recovery codes.
public sealed class PasswordRecoveryService
{
    internal Func<ResetPasswordRequest, CancellationToken, Task<ResetPasswordResponse>>? Transport { get; set; }

    public static void Validate(string username, string recoveryCode, string password, string confirmation)
    {
        if (string.IsNullOrWhiteSpace(username) || username.Length > 256 || username.Any(char.IsControl))
            throw new ArgumentException("请输入有效账号。");
        if (string.IsNullOrWhiteSpace(recoveryCode) || recoveryCode.Length > 512 || recoveryCode.Any(char.IsControl))
            throw new ArgumentException("请输入恢复码。");
        if (!PasswordPolicy.IsValidPassword(password)) throw new ArgumentException("密码至少填写一个字符，且不能超过允许长度。");
        if (!string.Equals(password, confirmation, StringComparison.Ordinal)) throw new ArgumentException("两次密码不一致。");
    }

    public async Task<PasswordRecoveryResult> ResetAsync(string server, string bindIp, string username,
        string recoveryCode, string password, string confirmation, CancellationToken ct = default)
    {
        Validate(username, recoveryCode, password, confirmation);
        var request = new ResetPasswordRequest { Username = username.Trim(), RecoveryCode = recoveryCode.Trim(), NewPassword = password };
        try
        {
            ResetPasswordResponse response;
            if (Transport != null) response = await Transport(request, ct);
            else
            {
                using var connection = await LanConnection.ConnectAsync(server, bindIp, ct);
                response = await connection.Api.ResetPasswordAsync(request, deadline: DateTime.UtcNow.AddSeconds(10), cancellationToken: ct);
            }
            return string.IsNullOrWhiteSpace(response.RecoveryCode) || response.RecoveryCode.Length > 512 || response.RecoveryCode.Any(char.IsControl)
                ? new(false, "missing-replacement-code") : new(true, "ok", response.RecoveryCode);
        }
        catch (RpcException e) when (e.StatusCode is StatusCode.Unauthenticated or StatusCode.PermissionDenied or StatusCode.NotFound)
        { return new(false, "invalid-code"); }
        catch (RpcException e) when (e.StatusCode == StatusCode.Unimplemented) { return new(false, "unsupported-server"); }
        catch (OperationCanceledException) { return new(false, "cancelled"); }
        catch (Exception e) when (e is RpcException or IOException or System.Net.Sockets.SocketException or System.Net.Http.HttpRequestException)
        { return new(false, "connection-failed"); }
    }
}

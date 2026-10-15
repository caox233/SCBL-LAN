using System;
namespace SplinterCellCNLauncher.Models;

public enum LoginStatus
{
    Success,
    InvalidPassword,
    UserNotFound,
    ConnectionFailed,
    ServerError
}

public sealed class LoginResult
{
    public LoginStatus Status { get; init; }
    public string Message { get; init; } = "";
    public string AccountId { get; init; } = "";

    public static LoginResult Success(string accountId = "") => new()
    {
        Status = LoginStatus.Success,
        Message = "登录成功。",
        AccountId = accountId
    };
}

public enum RegisterStatus
{
    Success,
    AlreadyExists,
    ConnectionFailed,
    ServerError
}

public sealed class RegisterResult
{
    public string RecoveryCode {get;init;} = "";
    public RegisterStatus Status { get; init; }
    public string Message { get; init; } = "";

    public static RegisterResult Success() => new()
    {
        Status = RegisterStatus.Success,
        Message = "注册成功。"
    };
}

using System.Security.Cryptography;
using System.Text;

using System;
namespace SplinterCellCNLauncher.Services;

public static class AccountIdService
{
    public static string CreateStableAccountId(string username)
    {
        // 当前服务端 LoginResponse 没有返回 account_id，所以先按用户名生成稳定 ID。
        // 后续如果服务端返回 account_id，这个函数就只作为备用。
        using MD5 md5 = MD5.Create();
        byte[] hash = md5.ComputeHash(Encoding.UTF8.GetBytes(username.Trim().ToLowerInvariant()));
        return new Guid(hash).ToString();
    }
}

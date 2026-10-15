using System.Reflection;
using System.Security.Cryptography;

namespace ScblLan;

// User-authorized local Ubisoft DLL, frozen at packaging time. No online download,
// signature-chain requirement on the player's PC, or synthetic CdKeys bypass.
internal static class BundledOriginalLoader
{
    internal const string Sha256 = "D098E458B935721478F618B9E2F2A0990EA0D9E907AA9DCAC0B5E78D99BB0B89";
    internal static byte[] Read()
    {
        using var stream = typeof(BundledOriginalLoader).Assembly.GetManifestResourceStream("SCBL.LAN.uplay_r1_loader.orig.dll")
            ?? throw new IOException("启动器缺少内置原版 DLL，请重新解压完整的启动器更新包。");
        if (stream.Length != 400912) throw new IOException("内置原版 DLL 大小校验失败。");
        var bytes = new byte[400912]; stream.ReadExactly(bytes);
        if (Convert.ToHexString(SHA256.HashData(bytes)) != Sha256)
            throw new IOException("内置原版 DLL 完整性校验失败。");
        return bytes;
    }
    internal static void Create(string target)
    {
        // Non-overwriting final move: an orig created by another process is never clobbered.
        string temp = target + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            File.WriteAllBytes(temp, Read());
            if (GameRuntime.Hash(temp) != Sha256 || !LoaderCompatibility.CanForward(temp))
                throw new IOException("内置原版 DLL 不是兼容的 x86 转发库。");
            File.Move(temp, target, false);
            if (GameRuntime.Hash(target) != Sha256) throw new IOException("原版 DLL 补齐后校验失败。");
        }
        finally { if (File.Exists(temp)) File.Delete(temp); }
    }
}

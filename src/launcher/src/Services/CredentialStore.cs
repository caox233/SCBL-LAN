using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using ScblLan;

namespace SplinterCellCNLauncher.Services;

/// <summary>Only passwords entered into this launcher: current-user DPAPI, bound to endpoint + username.</summary>
public sealed class CredentialStore
{
    private static readonly object Gate = new();
    private readonly string directory;
    public CredentialStore(string? root = null) => directory=Path.Combine(root ?? LogService.AppDataDir,"credentials");
    public static string Scope(string server,string username)
    {
        if(server.Length>512 || string.IsNullOrWhiteSpace(username) || username.Length>256 || username.Contains('\0'))throw new ArgumentException("Invalid credential identity.");
        var address=ServerAddress.Parse(server);
        return "SCBL-LAN-DPAPI-v1\n"+address.Host.ToLowerInvariant()+":"+address.Port+"\n"+username.Trim();
    }
    private string FilePath(string scope) => Path.Combine(directory,Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(scope)))+".bin");
    public string? Load(string server,string username)
    {
        string scope=Scope(server,username),path=FilePath(scope);
        lock(Gate) {
            if(!File.Exists(path))return null;
            using var stream=new FileStream(path,FileMode.Open,FileAccess.Read,FileShare.Read);
            if(stream.Length is <=0 or >32768)throw new IOException("保存的密码文件无效，请重新输入或取消记住密码。");
            byte[] encrypted=new byte[checked((int)stream.Length)];stream.ReadExactly(encrypted);
            byte[] plain=Transform(encrypted,scope,false);
            try{return new UTF8Encoding(false,true).GetString(plain);}
            finally{CryptographicOperations.ZeroMemory(plain);}
        }
    }
    public void Save(string server,string username,string password)
    {
        if(!PasswordPolicy.IsValidPassword(password))throw new ArgumentException("Empty, oversized or invalidly encoded password.");
        string scope=Scope(server,username),path=FilePath(scope);
        byte[] plain=Encoding.UTF8.GetBytes(password),encrypted;
        try{encrypted=Transform(plain,scope,true);}finally{CryptographicOperations.ZeroMemory(plain);}
        lock(Gate) {
            Directory.CreateDirectory(directory);
            if(!File.Exists(path) && Directory.EnumerateFiles(directory,"*.bin").Take(32).Count()>=32)
                throw new IOException("已保存 32 个登录身份；请取消记住密码以清理后再试。");
            string temp=path+"."+Guid.NewGuid().ToString("N")+".tmp";
            try{File.WriteAllBytes(temp,encrypted);File.Move(temp,path,true);}
            finally{if(File.Exists(temp))File.Delete(temp);}
        }
    }
    public void Remove(string server, string username)
    {
        string path=FilePath(Scope(server,username));
        lock(Gate) { if(File.Exists(path))File.Delete(path); }
    }
    public void Clear()
    {
        lock(Gate) {
            if(!Directory.Exists(directory))return;
            foreach(string path in Directory.EnumerateFiles(directory,"*.bin")) {
                string name=Path.GetFileNameWithoutExtension(path);
                if(name.Length==64 && name.All(Uri.IsHexDigit))File.Delete(path);
            }
        }
    }
    [StructLayout(LayoutKind.Sequential)] private struct Blob { public int Length; public IntPtr Data; }
    [DllImport("crypt32.dll",SetLastError=true,CharSet=CharSet.Unicode)]
    [return:MarshalAs(UnmanagedType.Bool)]
    private static extern bool CryptProtectData(ref Blob input,string? description,ref Blob entropy,IntPtr reserved,IntPtr prompt,uint flags,out Blob output);
    [DllImport("crypt32.dll",SetLastError=true)]
    [return:MarshalAs(UnmanagedType.Bool)]
    private static extern bool CryptUnprotectData(ref Blob input,IntPtr description,ref Blob entropy,IntPtr reserved,IntPtr prompt,uint flags,out Blob output);
    [DllImport("kernel32.dll")]private static extern IntPtr LocalFree(IntPtr memory);
    private static byte[] Transform(byte[] bytes,string scope,bool protect)
    {
        byte[] entropy=SHA256.HashData(Encoding.UTF8.GetBytes(scope));
        var inputPin=GCHandle.Alloc(bytes,GCHandleType.Pinned);var entropyPin=GCHandle.Alloc(entropy,GCHandleType.Pinned);
        Blob output=default;
        try {
            var input=new Blob{Length=bytes.Length,Data=inputPin.AddrOfPinnedObject()};var optional=new Blob{Length=entropy.Length,Data=entropyPin.AddrOfPinnedObject()};
            // UI forbidden; NEVER use CRYPTPROTECT_LOCAL_MACHINE (which admits other Windows users).
            bool ok=protect?CryptProtectData(ref input,null,ref optional,IntPtr.Zero,IntPtr.Zero,1,out output)
                           :CryptUnprotectData(ref input,IntPtr.Zero,ref optional,IntPtr.Zero,IntPtr.Zero,1,out output);
            if(!ok || output.Data==IntPtr.Zero || output.Length is <=0 or >32768)
                throw new CryptographicException("无法加密或解密保存的密码，请在当前 Windows 用户下重新输入。");
            byte[] result=new byte[output.Length];Marshal.Copy(output.Data,result,0,result.Length);return result;
        } finally {
            if(output.Data!=IntPtr.Zero){if(output.Length is >0 and <=32768)Marshal.Copy(new byte[output.Length],0,output.Data,output.Length);LocalFree(output.Data);}
            inputPin.Free();entropyPin.Free();CryptographicOperations.ZeroMemory(entropy);
        }
    }
}

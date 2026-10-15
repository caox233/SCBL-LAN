using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace SplinterCellCNLauncher.Services;

// Trusted offline deployment + local consistency only; not PKI or remote attestation.
// Immutable per prepared authentication/launch. No global fallback or live policy reload.
public sealed class ReleasePackage
{
    private static readonly (string Role,string Path)[] Roles=[("launcher","SCBL.LAN.exe"),("main","runtime/uplay_r1_loader.dll"),("gameplay","runtime/scbl_gameplay.dll"),("server","runtime/SCBL.LAN.Server.exe")];
    public string PackageRoot {get;}
    public string ReleaseId {get;}
    public string CompatId {get;}
    public string ManifestSha256 {get;}
    private readonly string mainHash;
    private readonly long mainBytes;
    private ReleasePackage(string root,string release,string compat,string digest,string hash,long bytes)
    {PackageRoot=root;ReleaseId=release;CompatId=compat;ManifestSha256=digest;mainHash=hash;mainBytes=bytes;}
    public static ReleasePackage Current()=>Verify(Environment.ProcessPath??throw Invalid());
    private static IOException Invalid()=>new("PACKAGE_CATALOG_INVALID: reinstall the complete matched offline package.");
    private static string Hash(Stream stream)=>Convert.ToHexString(SHA256.HashData(stream)).ToLowerInvariant();
    private static void Plain(string path)
    {
        for(string? p=Path.GetFullPath(path);p!=null;p=Path.GetDirectoryName(p))
            if((File.GetAttributes(p)&FileAttributes.ReparsePoint)!=0)throw Invalid();
        if((File.GetAttributes(path)&FileAttributes.Directory)!=0)throw Invalid();
    }
    private static void Shape(JsonElement node,params string[] fields)
    {
        if(node.ValueKind!=JsonValueKind.Object)throw Invalid();
        var seen=new HashSet<string>(StringComparer.Ordinal);
        foreach(var item in node.EnumerateObject())if(!fields.Contains(item.Name,StringComparer.Ordinal)||!seen.Add(item.Name))throw Invalid();
        if(seen.Count!=fields.Length)throw Invalid();
    }
    private static bool Id(string? s)=>s is {Length:>0 and <=64}&&s.All(c=>char.IsAsciiLetterOrDigit(c)||c is '.' or '_' or '-');
    private static bool Digest(string? s)=>s is {Length:64}&&s.All(c=>c is >= '0' and <= '9' or >= 'a' and <= 'f');
    public static ReleasePackage Verify(string executable)
    {
        // The public seam receives an executable, never an arbitrary catalog/root claim.
        executable=Path.GetFullPath(executable);Plain(executable);
        if(!Path.GetFileName(executable).Equals(Roles[0].Path,StringComparison.OrdinalIgnoreCase))throw Invalid();
        string root=Path.GetDirectoryName(executable)!;
        string catalog=Path.Combine(root,"package","RELEASE-CATALOG.json");Plain(catalog);
        byte[] raw;
        using(var input=File.OpenRead(catalog)) {
            if(input.Length is <=0 or >16384)throw Invalid();raw=new byte[(int)input.Length];input.ReadExactly(raw);if(input.ReadByte()!=-1)throw Invalid();
        }
        try {
            using var doc=JsonDocument.Parse(raw,new(){MaxDepth=6});var value=doc.RootElement;
            Shape(value,"schema","release_id","compat_id","components");
            string release=value.GetProperty("release_id").GetString()!,compat=value.GetProperty("compat_id").GetString()!;
            if(value.GetProperty("schema").GetUInt32()!=1||!Id(release)||!Id(compat)||release!=ReleaseBuildIdentity.ReleaseId||compat!=ReleaseBuildIdentity.CompatId)throw Invalid();
            // Real literal generated from the same prebuild JSON as native binaries. It remains reachable in the launcher PE.
            GC.KeepAlive(ReleaseBuildIdentity.LauncherMarker);
            var rows=value.GetProperty("components");if(rows.ValueKind!=JsonValueKind.Array||rows.GetArrayLength()!=4)throw Invalid();
            string main="";long bytes=0;
            for(int i=0;i<4;i++) {
                var row=rows[i];Shape(row,"role","path","release_id","compat_id","bytes","sha256");
                string hash=row.GetProperty("sha256").GetString()!;long size=row.GetProperty("bytes").GetInt64();
                if(row.GetProperty("role").GetString()!=Roles[i].Role||row.GetProperty("path").GetString()!=Roles[i].Path||row.GetProperty("release_id").GetString()!=release||row.GetProperty("compat_id").GetString()!=compat||!Digest(hash)||size<=0)throw Invalid();
                VerifyFile(Path.Combine(root,Roles[i].Path),Roles[i].Role,release,compat,hash,size);
                if(i==1){main=hash;bytes=size;}
            }
            return new(root,release,compat,Convert.ToHexString(SHA256.HashData(raw)).ToLowerInvariant(),main,bytes);
        }catch(Exception e) when(e is JsonException or InvalidOperationException or FormatException or OverflowException or KeyNotFoundException){throw Invalid();}
    }
    private static void VerifyFile(string path,string role,string release,string compat,string hash,long size)
    {
        Plain(path);using var input=File.OpenRead(path);if(input.Length!=size)throw Invalid();
        string expected=$"SCBL_RELEASE_V1|{role}|{release}|{compat}|END_SCBl";
        byte[] prefix=Encoding.ASCII.GetBytes("SCBL_RELEASE_V1|"),widePrefix=Encoding.Unicode.GetBytes("SCBL_RELEASE_V1|");
        byte[] ownPrefix=Encoding.ASCII.GetBytes($"SCBL_RELEASE_V1|{role}|"),wideOwnPrefix=Encoding.Unicode.GetBytes($"SCBL_RELEASE_V1|{role}|");
        byte[] wanted=Encoding.ASCII.GetBytes(expected),wideWanted=Encoding.Unicode.GetBytes(expected);
        using var digest=IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
        byte[] buffer=new byte[65536+512];int tail=0,markers=0;long total=0;
        while(true) {
            int count=input.Read(buffer,tail,65536);digest.AppendData(buffer,tail,count);total+=count;
            int available=tail+count,scan=count==0?available:Math.Max(0,available-512);
            for(int offset=0;offset<scan;offset++) {
                var remaining=buffer.AsSpan(offset,available-offset);
                for(int encoding=0;encoding<2;encoding++) {
                    bool wide=encoding==1;
                    if(remaining.StartsWith(wide?wideOwnPrefix:ownPrefix)){if(!remaining.StartsWith(wide?wideWanted:wanted))throw Invalid();markers++;}
                    if(remaining.StartsWith(wide?widePrefix:prefix)&&CompleteRole(remaining,wide) is string found&&found!=role)throw Invalid();
                }
            }
            if(count==0)break;tail=available-scan;Buffer.BlockCopy(buffer,scan,buffer,0,tail);
        }
        if(markers!=1||total!=size||Convert.ToHexString(digest.GetHashAndReset()).ToLowerInvariant()!=hash)throw Invalid();
    }
    private static string? CompleteRole(ReadOnlySpan<byte> bytes,bool wide)
    {
        var text=new StringBuilder();int stride=wide?2:1;
        for(int i=0;i<Math.Min(bytes.Length,400);i+=stride) {
            byte b=bytes[i];if(b==0||b>127||(wide&&(i+1>=bytes.Length||bytes[i+1]!=0)))return null;text.Append((char)b);
            if(text.ToString().EndsWith("|END_SCBl",StringComparison.Ordinal))break;
        }
        var parts=text.ToString().Split('|');return parts.Length==5&&parts[0]=="SCBL_RELEASE_V1"&&parts[4]=="END_SCBl"&&Id(parts[2])&&Id(parts[3])&&Roles.Any(r=>r.Role==parts[1])?parts[1]:null;
    }
    public void VerifyDeployedMain(string main)
    {
        Plain(main);using var input=File.OpenRead(main);if(input.Length!=mainBytes||Hash(input)!=mainHash)throw Invalid();
    }
}

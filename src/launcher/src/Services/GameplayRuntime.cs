using System.Buffers.Binary;
using System.Reflection.PortableExecutable;
using System.Text;
using ScblLan;

namespace SplinterCellCNLauncher.Services;

public static class RuntimePaths
{
    // Runtime location is independent of the product version. User data stays separate.
    public static string Root => Path.Combine(AppContext.BaseDirectory,"runtime");
}

public static class GameplayRuntime
{
    public static double NormalizeFov(double value) => double.IsFinite(value) ? Math.Clamp(value,1.0,1.3) : 1.2;
    public static string Normalize(string? mode) => string.Equals(mode,"MOD",StringComparison.OrdinalIgnoreCase)?"MOD":"Official";
    public static void ValidateServerMode(string mode,bool officialOnly)
    {
        if(Normalize(mode)=="MOD" && officialOnly) throw new IOException("房主服务端仅支持官方模式；请让房主升级到支持 MOD 的 SCBL LAN 版本，或改选官方模式。");
    }
    public static string? ValidateForLaunch(string mode,string runtimeRoot)
    {
        if(Normalize(mode)!="MOD") return null;
        string directory=Path.GetFullPath(runtimeRoot);
        string dll=Path.Combine(directory,"scbl_gameplay.dll");
        if(!File.Exists(dll) || !File.Exists(dll+".sha256")) throw new IOException("MOD 模块不完整：缺少本版 scbl_gameplay.dll 或校验文件。不会静默改为官方模式。");
        string expected=File.ReadAllText(dll+".sha256").Trim();
        if(expected.Length!=64 || !GameRuntime.Hash(dll).Equals(expected,StringComparison.OrdinalIgnoreCase)) throw new IOException("MOD 模块校验失败，请使用完整的同版本 LAN 开发包。");
        try {
            using var stream=File.OpenRead(dll);using var pe=new PEReader(stream);
            if(pe.PEHeaders.CoffHeader.Machine!=Machine.I386 || (pe.PEHeaders.CoffHeader.Characteristics&Characteristics.Dll)==0)throw new BadImageFormatException();
            var exports=pe.PEHeaders.PEHeader!.ExportTableDirectory;
            var table=pe.GetSectionData(exports.RelativeVirtualAddress).GetContent(0,40).AsSpan();
            uint count=BinaryPrimitives.ReadUInt32LittleEndian(table[24..]);int names=checked((int)BinaryPrimitives.ReadUInt32LittleEndian(table[32..]));
            if(count==0 || count>4096)throw new BadImageFormatException();
            var pointers=pe.GetSectionData(names).GetContent(0,checked((int)count*4)).AsSpan();
            var found=new HashSet<string>(StringComparer.Ordinal);
            for(int i=0;i<count;i++) {
                int rva=checked((int)BinaryPrimitives.ReadUInt32LittleEndian(pointers[(i*4)..]));
                var block=pe.GetSectionData(rva);var bytes=block.GetContent(0,Math.Min(block.Length,160)).AsSpan();int end=bytes.IndexOf((byte)0);
                if(end>=0)found.Add(Encoding.ASCII.GetString(bytes[..end]));
            }
            if(!new[]{"scbl_plugin_initialize","scbl_plugin_shutdown","scbl_plugin_handle_event"}.All(found.Contains))throw new BadImageFormatException();
        } catch(Exception e) when(e is BadImageFormatException or ArgumentException or OverflowException or InvalidOperationException) {
            throw new IOException("MOD 模块不是兼容的 x86 SCBL gameplay 插件。",e);
        }
        return directory;
    }
}

using System.Buffers.Binary;
using System.Reflection.PortableExecutable;
using System.Text;

namespace ScblLan;

// ABI screening only, not signature verification or proof of arbitrary third-party compatibility.
internal static class LoaderCompatibility
{
    internal static bool CanForward(string path)
    {
        try
        {
            if(new FileInfo(path).Length>64*1024*1024)return false;
            byte[] image=File.ReadAllBytes(path);
            if(RefersToOwnForwarder(image))return false;
            using var stream=new MemoryStream(image,false);using var pe=new PEReader(stream);
            if(pe.PEHeaders.CoffHeader.Machine!=Machine.I386 ||
               (pe.PEHeaders.CoffHeader.Characteristics&Characteristics.Dll)==0 || pe.PEHeaders.PEHeader==null)return false;
            var directory=pe.PEHeaders.PEHeader.ExportTableDirectory;
            if(directory.RelativeVirtualAddress==0 || directory.Size<40)return false;
            var table=pe.GetSectionData(directory.RelativeVirtualAddress).GetContent(0,40).AsSpan();
            uint count=BinaryPrimitives.ReadUInt32LittleEndian(table[24..]);
            if(count==0 || count>4096)return false;
            int names=checked((int)BinaryPrimitives.ReadUInt32LittleEndian(table[32..]));
            var pointers=pe.GetSectionData(names).GetContent(0,checked((int)count*4)).AsSpan();
            var found=new HashSet<string>(StringComparer.Ordinal);
            for(int i=0;i<count;i++)
            {
                int rva=checked((int)BinaryPrimitives.ReadUInt32LittleEndian(pointers[(i*4)..]));
                var block=pe.GetSectionData(rva);var name=block.GetContent(0,Math.Min(block.Length,160)).AsSpan();int end=name.IndexOf((byte)0);
                if(end>=0)found.Add(Encoding.ASCII.GetString(name[..end]));
            }
            return found.Contains("UPLAY_Startup") && found.Contains("UPLAY_GetLastError");
        }
        catch(Exception e) when(e is IOException or UnauthorizedAccessException or BadImageFormatException or ArgumentException or OverflowException or InvalidOperationException){return false;}
    }
    private static bool RefersToOwnForwarder(byte[] image)
    {
        // Both LAN/public SCBL hooks forward into this exact name. Never copy one into .orig.dll.
        foreach(string marker in new[]{"uplay_r1_loader.orig.dll","SCBL_HOOKS_CONFIG="})
            foreach(var encoding in new[]{Encoding.ASCII,Encoding.Unicode})
                if(encoding.GetString(image).Contains(marker,StringComparison.OrdinalIgnoreCase))return true;
        return false;
    }
}

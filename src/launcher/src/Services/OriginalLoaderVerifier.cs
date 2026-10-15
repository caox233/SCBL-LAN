using System.Reflection.PortableExecutable;
using System.Runtime.InteropServices;
using System.Security.Cryptography.X509Certificates;

namespace ScblLan;

/// <summary>Version-resource CompanyName is optional and is not proof of publisher identity.</summary>
public static class OriginalLoaderVerifier
{
    public static bool IsAuthentic(string path)
    {
        try
        {
            using var stream = File.Open(path, FileMode.Open, FileAccess.Read, FileShare.Read);
            using var pe = new PEReader(stream);
            if (pe.PEHeaders.CoffHeader.Machine != Machine.I386 ||
                (pe.PEHeaders.CoffHeader.Characteristics & Characteristics.Dll) == 0) return false;
            if (VerifySignature(path) != 0) return false;
            // X509CertificateLoader accepts certificate encodings, not a signed PE image.
            // Use the PE extraction API only after WinVerifyTrust has verified the signature.
#pragma warning disable SYSLIB0057
            using var certificate = X509Certificate.CreateFromSignedFile(path);
#pragma warning restore SYSLIB0057
            using var signer = X509CertificateLoader.LoadCertificate(certificate.GetRawCertData());
            string publisher = signer.GetNameInfo(X509NameType.SimpleName, false);
            return publisher.Equals("UBISOFT ENTERTAINMENT INC.", StringComparison.OrdinalIgnoreCase) ||
                   publisher.Equals("UBISOFT ENTERTAINMENT", StringComparison.OrdinalIgnoreCase);
        }
        catch (Exception e) when (e is IOException or BadImageFormatException or System.Security.Cryptography.CryptographicException)
        { return false; }
    }

    private static int VerifySignature(string path)
    {
        var file = new TrustFile { Size = (uint)Marshal.SizeOf<TrustFile>(), Path = Marshal.StringToCoTaskMemUni(Path.GetFullPath(path)) };
        IntPtr filePointer = Marshal.AllocCoTaskMem(Marshal.SizeOf<TrustFile>());
        var data = new TrustData { Size = (uint)Marshal.SizeOf<TrustData>(), UiChoice = 2, UnionChoice = 1,
            File = filePointer, StateAction = 1, ProviderFlags = 0x1010 };
        // Cache-only Authenticode validation: pure LAN must not require Internet certificate retrieval.
        // Offline mode does not claim a fresh online revocation check.
        var action = new Guid("00AAC56B-CD44-11D0-8CC2-00C04FC295EE");
        try { Marshal.StructureToPtr(file, filePointer, false); return WinVerifyTrust(new IntPtr(-1), ref action, ref data); }
        finally
        {
            data.StateAction = 2;
            WinVerifyTrust(new IntPtr(-1), ref action, ref data);
            Marshal.FreeCoTaskMem(filePointer); Marshal.FreeCoTaskMem(file.Path);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct TrustFile { public uint Size; public IntPtr Path; public IntPtr File; public IntPtr KnownSubject; }
    [StructLayout(LayoutKind.Sequential)]
    private struct TrustData
    {
        public uint Size; public IntPtr PolicyCallback; public IntPtr SipClient; public uint UiChoice;
        public uint RevocationChecks; public uint UnionChoice; public IntPtr File; public uint StateAction;
        public IntPtr StateData; public IntPtr Url; public uint ProviderFlags; public uint UiContext; public IntPtr SignatureSettings;
    }
    [DllImport("wintrust.dll", ExactSpelling=true, PreserveSig=true)]
    private static extern int WinVerifyTrust(IntPtr window, ref Guid action, ref TrustData data);
}

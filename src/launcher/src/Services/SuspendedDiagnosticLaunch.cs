using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
namespace SplinterCellCNLauncher.Services;

// Opt-in bootstrap only. Normal launching still uses Process.Start unchanged.
internal sealed class SuspendedDiagnosticLaunch : IDisposable
{
    private IntPtr thread;
    public Process Process { get; }
    private SuspendedDiagnosticLaunch(Process p,IntPtr t){Process=p;thread=t;}
    public static SuspendedDiagnosticLaunch Start(ProcessStartInfo info)
    {
        if(info.UseShellExecute||info.ArgumentList.Count!=0||!string.IsNullOrEmpty(info.Arguments))throw new ArgumentException("Diagnostic bootstrap expects the existing no-argument launch contract.");
        string environment=string.Join('\0',info.Environment.OrderBy(x=>x.Key,StringComparer.OrdinalIgnoreCase).Select(x=>x.Key+"="+x.Value))+"\0\0";
        IntPtr env=Marshal.StringToHGlobalUni(environment);
        try {
            var si=new StartupInfo{Size=Marshal.SizeOf<StartupInfo>()};
            if(!CreateProcessW(info.FileName,new StringBuilder("\""+info.FileName+"\""),IntPtr.Zero,IntPtr.Zero,false,0x404u|(info.CreateNoWindow?0x08000000u:0),env,info.WorkingDirectory,ref si,out var pi))throw new Win32Exception(Marshal.GetLastWin32Error());
            try{return new(Process.GetProcessById(pi.Pid),pi.Thread);}
            catch{ResumeThread(pi.Thread);CloseHandle(pi.Thread);throw;}
            finally{CloseHandle(pi.Process);}
        } finally{Marshal.FreeHGlobal(env);}
    }
    public void Resume(){if(thread==IntPtr.Zero)return;if(ResumeThread(thread)==uint.MaxValue)throw new Win32Exception(Marshal.GetLastWin32Error());CloseHandle(thread);thread=IntPtr.Zero;}
    public void Dispose(){if(thread!=IntPtr.Zero){ResumeThread(thread);CloseHandle(thread);thread=IntPtr.Zero;}}
    [StructLayout(LayoutKind.Sequential,CharSet=CharSet.Unicode)]private struct StartupInfo{public int Size;public string? Reserved,Desktop,Title;public int X,Y,XSize,YSize,XChars,YChars,Fill,Flags;public short Show,ReservedBytes;public IntPtr Reserved2,Input,Output,Error;}
    [StructLayout(LayoutKind.Sequential)]private struct ProcessInfo{public IntPtr Process,Thread;public int Pid,Tid;}
    [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)]private static extern bool CreateProcessW(string application,StringBuilder command,IntPtr processAttributes,IntPtr threadAttributes,bool inherit,uint flags,IntPtr environment,string directory,ref StartupInfo startup,out ProcessInfo process);
    [DllImport("kernel32.dll",SetLastError=true)]private static extern uint ResumeThread(IntPtr thread);
    [DllImport("kernel32.dll")]private static extern bool CloseHandle(IntPtr handle);
}

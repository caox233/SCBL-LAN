using System.Diagnostics;
using System.Net.Sockets;
namespace SplinterCellCNLauncher.Services;

public static class PortOwnerDiagnostics
{
    private static readonly object ownershipGate=new();
    private static Process? diagnosticProcess;
    private static Task diagnosticReads=Task.CompletedTask;
    // BOUND outbound reservations may deny bind even when no service is LISTENING.
    public static string Describe(int port, ProtocolType protocol)
    {
        lock(ownershipGate)
        {
        try
        {
            // A retained owner is drained before a new query can replace it.
            StopOwned();
            var psi=new ProcessStartInfo(Path.Combine(Environment.SystemDirectory,"netstat.exe")) {
                UseShellExecute=false,CreateNoWindow=true,WindowStyle=ProcessWindowStyle.Hidden,
                RedirectStandardOutput=true,RedirectStandardError=true };
            psi.ArgumentList.Add(protocol==ProtocolType.Tcp?"-anoq":"-ano");
            psi.ArgumentList.Add("-p");psi.ArgumentList.Add(protocol==ProtocolType.Tcp?"tcp":"udp");
            var p=Process.Start(psi);if(p==null)throw new IOException("端口诊断进程创建失败。");
            diagnosticProcess=p;
            Task<string> output=p.StandardOutput.ReadToEndAsync();diagnosticReads=output;
            Task<string> error=p.StandardError.ReadToEndAsync();diagnosticReads=Task.WhenAll(output,error);
            if(!p.WaitForExit(2500))throw new TimeoutException("端口诊断进程等待超时。");
            diagnosticReads.WaitAsync(TimeSpan.FromSeconds(3)).GetAwaiter().GetResult();
            string text=output.Result,details=error.Result;
            int exitCode=p.ExitCode;
            StopOwned();
            if(exitCode!=0)throw new IOException("端口诊断进程失败（退出码 "+exitCode+"）。"+(string.IsNullOrWhiteSpace(details)?"":" "+details.Trim()));
            var matches=new HashSet<string>();
            foreach(string line in text.Split('\n'))
            {
                var parts=line.Split((char[]?)null,StringSplitOptions.RemoveEmptyEntries);
                if(parts.Length<4 || !parts[1].EndsWith(":"+port,StringComparison.Ordinal) || !int.TryParse(parts[^1],out int pid) || pid<=0)continue;
                string name="进程";
                try{using var owner=Process.GetProcessById(pid);name=owner.ProcessName;}catch{}
                string state=protocol==ProtocolType.Tcp && parts.Length>=5?parts[3]:"UDP";
                matches.Add($"{name}（PID {pid}，{parts[1]}，{state}）");
            }
            return matches.Count==0?"":"\n本机同端口占用："+string.Join("；",matches.Take(4))+"。请在确认后自行退出对应程序或释放连接，再重试。";
        }
        catch(Exception diagnosticFailure)
        {
            try {StopOwned();}
            catch(Exception cleanupFailure)
            {
                diagnosticFailure=new AggregateException("Owned port diagnostic cleanup did not complete successfully.",diagnosticFailure,cleanupFailure);
            }
            // Preserve the caller's bind error, but never report failed cleanup as an empty success.
            return "\n本机端口占用诊断未完成："+diagnosticFailure.Message+" 请稍后重试；未终止其他程序。";
        }
        }
    }
    private static void StopOwned()
    {
        var owned=diagnosticProcess;
        if(owned==null)return;
        if(!owned.HasExited)
        {
            owned.Kill(false);
            if(!owned.WaitForExit(3000))throw new IOException("The owned port diagnostic has not exited; retry the diagnostic.");
        }
        Exception? readFailure=null;
        try {diagnosticReads.WaitAsync(TimeSpan.FromSeconds(3)).GetAwaiter().GetResult();}
        catch(TimeoutException) when(!diagnosticReads.IsCompleted) {throw;}
        catch(Exception failure) {readFailure=diagnosticReads.Exception?.Flatten() ?? failure;}
        owned.Dispose();
        diagnosticProcess=null;
        diagnosticReads=Task.CompletedTask;
        if(readFailure!=null)System.Runtime.ExceptionServices.ExceptionDispatchInfo.Capture(readFailure).Throw();
    }
}

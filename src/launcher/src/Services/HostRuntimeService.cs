using System.Diagnostics;
using ScblLan;
namespace SplinterCellCNLauncher.Services;
public sealed class HostRuntimeService : IDisposable
{
    private Process? process;
    private Task hostReads=Task.CompletedTask;
    private bool hostStarted;
    private Process? initializationProcess;
    private Task initializationReads=Task.CompletedTask;
    private string localIp="";
    private bool smokePorts;
    public int ApiPort { get; private set; } = 50051;
    public string PortNotice { get; private set; } = "";
    public bool IsRunning {get {try{return process!=null && !process.HasExited;}catch{return false;}}}
    public async Task InitializeAsync(string executable,string data,string ip,bool smoke=false)
    {
        if(!File.Exists(executable)) throw new IOException("缺少本开发包服务端 runtime/SCBL.LAN.Server.exe。");
        string expected=File.ReadAllText(executable+".sha256").Trim().Split(' ',StringSplitOptions.RemoveEmptyEntries)[0];
        if(!GameRuntime.Hash(executable).Equals(expected,StringComparison.OrdinalIgnoreCase)) throw new IOException("服务端文件校验失败。");
        if(initializationProcess!=null) throw new IOException("The previous owned initializer has not drained; retry stopping it before initializing again.");
        localIp=ip;smokePorts=smoke;
        Directory.CreateDirectory(data);
        if(File.Exists(Path.Combine(data,"service.toml"))) return;
        var psi=Info(executable,data);
        psi.ArgumentList.Add("--init-lan");psi.ArgumentList.Add(ip);
        if(smoke) psi.ArgumentList.Add("--smoke-ports");
        var p=Process.Start(psi) ?? throw new IOException("服务端初始化进程创建失败。");
        initializationProcess=p;
        try
        {
            Task<string> so=p.StandardOutput.ReadToEndAsync();
            initializationReads=so;
            Task<string> se=p.StandardError.ReadToEndAsync();
            initializationReads=Task.WhenAll(so,se);
            using var timeout=new CancellationTokenSource(TimeSpan.FromSeconds(20));
            await p.WaitForExitAsync(timeout.Token);
            await initializationReads.WaitAsync(TimeSpan.FromSeconds(3));
            string output=so.Result,error=se.Result;
            int exitCode=p.ExitCode;
            StopInitialization();
            File.WriteAllText(Path.Combine(data,"init.stdout.txt"),output);File.WriteAllText(Path.Combine(data,"init.stderr.txt"),error);
            if(exitCode!=0) throw new IOException("服务端初始化失败，请查看本轮独立数据目录的 init.stderr.txt。");
        }
        catch(Exception initializationFailure)
        {
            try {StopInitialization();}
            catch(Exception cleanupFailure)
            {
                if(ReferenceEquals(initializationFailure,cleanupFailure))throw;
                throw new AggregateException("Owned initializer failed and cleanup did not complete successfully.",initializationFailure,cleanupFailure);
            }
            throw;
        }
    }
    private static ProcessStartInfo Info(string exe,string data)
    {
        var psi=new ProcessStartInfo(exe) {WorkingDirectory=data,UseShellExecute=false,CreateNoWindow=true,WindowStyle=ProcessWindowStyle.Hidden,RedirectStandardOutput=true,RedirectStandardError=true};
        psi.ArgumentList.Add("--data-dir");psi.ArgumentList.Add(data);return psi;
    }
    public void Start(string exe,string data)
    {
        if(process!=null && !hostStarted)throw new IOException("The previous owned host startup has not drained; retry stopping it before starting again.");
        if(IsRunning)return;
        if(process!=null)Stop();
        var plan = HostPortPlan.Create(localIp, smokePorts);
        ApiPort = plan.ApiPort; PortNotice = plan.Notice;
        var psi = Info(exe, data);
        var owned=Process.Start(psi) ?? throw new IOException("服务端创建失败。");
        process=owned;
        try
        {
            var outputEnded=new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            var errorEnded=new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            owned.OutputDataReceived+=(_,e)=>{if(e.Data!=null)Append(data,"server.stdout.txt",e.Data);else outputEnded.TrySetResult();};
            owned.ErrorDataReceived+=(_,e)=>{if(e.Data!=null)Append(data,"server.stderr.txt",e.Data);else errorEnded.TrySetResult();};
            // Register only successfully begun readers; a throwing Begin has no EOF obligation.
            owned.BeginOutputReadLine();hostReads=outputEnded.Task;
            owned.BeginErrorReadLine();hostReads=Task.WhenAll(outputEnded.Task,errorEnded.Task);
            hostStarted=true;
        }
        catch(Exception startupFailure)
        {
            try {Stop();}
            catch(Exception cleanupFailure)
            {
                throw new AggregateException("Owned host startup failed and cleanup did not complete successfully.",startupFailure,cleanupFailure);
            }
            throw;
        }
    }
    private static void Append(string dir,string file,string line) {try{File.AppendAllText(Path.Combine(dir,file),line+Environment.NewLine);}catch{}}
    public async Task<bool> WaitReadyAsync(string server,string bind,TimeSpan timeout)
    {
        using var stop=new CancellationTokenSource(timeout);
        while(!stop.IsCancellationRequested && IsRunning)
        {
            try {using var c=await LanConnection.ConnectAsync(server,bind,stop.Token);return c.Capabilities.Ready && IsRunning;}
            catch(OperationCanceledException){return false;}
            catch {try{await Task.Delay(200,stop.Token);}catch(OperationCanceledException){return false;}}
        }
        return false;
    }
    private void StopInitialization()
    {
        var owned=initializationProcess;
        if(owned==null)return;
        // A failed termination or read timeout retains the same handle/tasks for Stop() to retry.
        if(!owned.HasExited)
        {
            owned.Kill(false);
            if(!owned.WaitForExit(3000))throw new IOException("The owned initializer has not exited; retry stopping it.");
        }
        Exception? readFailure=null;
        try {initializationReads.WaitAsync(TimeSpan.FromSeconds(3)).GetAwaiter().GetResult();}
        catch(TimeoutException) when(!initializationReads.IsCompleted) {throw;}
        catch(Exception failure) {readFailure=initializationReads.Exception?.Flatten() ?? failure;}
        owned.Dispose();
        if(ReferenceEquals(initializationProcess,owned))
        {
            initializationProcess=null;
            initializationReads=Task.CompletedTask;
        }
        if(readFailure!=null)System.Runtime.ExceptionServices.ExceptionDispatchInfo.Capture(readFailure).Throw();
    }
    public void Stop()
    {
        StopInitialization();
        var owned=process;
        if(owned==null)return;
        // Retain the owned handle if termination fails or has not completed, so a later stop can retry.
        if(!owned.HasExited)
        {
            owned.Kill(false);
            if(!owned.WaitForExit(3000))throw new IOException("The owned host process has not exited; retry stopping it.");
        }
        hostReads.WaitAsync(TimeSpan.FromSeconds(3)).GetAwaiter().GetResult();
        owned.Dispose();
        if(ReferenceEquals(process,owned))
        {
            process=null;
            hostReads=Task.CompletedTask;
            hostStarted=false;
        }
    }
    public void Dispose()=>Stop();
}

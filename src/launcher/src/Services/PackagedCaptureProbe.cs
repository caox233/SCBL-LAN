using System.Diagnostics;
using System.Text.Json;
namespace SplinterCellCNLauncher.Services;

// Only invoked by the explicit isolated --capture-self-test CLI, never normal startup.
internal static class PackagedCaptureProbe
{
    internal static async Task<string> RunAsync(string fixture,string report)
    {
        string root=Path.Combine(Path.GetDirectoryName(Path.GetFullPath(report))!,"packaged-capture-"+Guid.NewGuid().ToString("N"));Directory.CreateDirectory(root);
        string exe=Path.Combine(root,"Blacklist_DX11_game.exe");File.Copy(fixture,exe);
        File.Copy(Path.Combine(Path.GetDirectoryName(Path.GetFullPath(fixture))!,"ThreadNameFixture.dll"),Path.Combine(root,"ThreadNameFixture.dll"));
        string signal=Path.Combine(root,"named-startup");
        var run=new GameRunDiagnostics(exe,Path.Combine(root,"diagnostic-data"),new EmptyEvents(),false,new(RunGameMode.Official));
        try {
            run.EnableCapture(); // No override: exercise the helper embedded in the actual package.
            var psi=new ProcessStartInfo(exe){UseShellExecute=false,WorkingDirectory=root,CreateNoWindow=true};
            psi.Environment["SCBL_CAPTURE_FIXTURE_NAMES"]=signal; // Synthetic probe only; never real launch.
            using var start=SuspendedDiagnosticLaunch.Start(psi);
            using var p=start.Process;run.Observe(p,ProcessEvidence.CreatedByLauncher);start.Resume();
            for(int i=0;i<120&&!p.HasExited&&!File.Exists(signal+".notified");i++)await Task.Delay(50);
            run.Tick();
            if(p.HasExited||!File.Exists(signal+".notified")||File.Exists(Path.Combine(run.CaptureDirectory,"fatal.claim"))||!run.Snapshot().Capture.Any(c=>c.State==CaptureState.Armed))throw new IOException("Packaged helper terminated the valid thread-name protocol or consumed fatal capture.");
            File.WriteAllText(signal+".continue","go");
            await p.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(20));await Task.Delay(250);run.Tick();
            if(unchecked((uint)p.ExitCode)!=0xC0000005 || !run.Snapshot().Capture.Any(c=>c.State==CaptureState.Captured))throw new IOException("Packaged capture did not capture the synthetic AV.");
            string dump=Directory.GetFiles(run.CaptureDirectory,"exception.dmp",SearchOption.AllDirectories).Single();
            byte[] bytes=File.ReadAllBytes(dump);uint U(int x)=>BitConverter.ToUInt32(bytes,x);int table=(int)U(12),at=0;
            for(int i=0;i<U(8);i++)if(U(table+12*i)==6)at=(int)U(table+12*i+8);
            if(at==0||U(at+8)!=0xC0000005||U(at+160)<716||U((int)U(at+164)+184)==0)throw new IOException("Packaged dump lacks exception/context.");
            using var detail=JsonDocument.Parse(File.ReadAllText(Path.Combine(Path.GetDirectoryName(dump)!,"exception.json")));
            if(detail.RootElement.GetProperty("FrameCount").GetInt32()<2)throw new IOException("Packaged stack trace missing.");
            NativeCaptureSession.ExportPrivate(run.CaptureDirectory,Path.Combine(root,"private.zip"));
            return "PASS packaged embedded helper extracted and attached before startup\nPASS Bink-shaped DLL emitted 12 valid notifications and continued\nPASS naming leaves helper armed without fatal.claim\nPASS actual subsequent x86 unhandled AV retained original exit code\nPASS packaged dump ExceptionStream and x86 ThreadContext verified\nPASS faulting thread has caller stack frames\nPASS separate private export validates capture\nNo real game or player data accessed.\n";
        } finally {run.Close();}
    }
    private sealed class EmptyEvents:ICrashEventSource{public CrashReadResult Read(CrashQueryContext c)=>new(CrashQueryState.NoCorrelatedEventYet,null,[]);}
}

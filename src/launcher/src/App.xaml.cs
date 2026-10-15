using System.Threading;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;
namespace SplinterCellCNLauncher;
public partial class App : Application
{
    private Mutex? mutex;
    private bool owns;
    internal static bool PreviewMode {get;private set;}
    internal static bool HostCheckMode {get;private set;}
    protected override async void OnStartup(StartupEventArgs e)
    {
        AppContext.SetSwitch("System.Net.Http.SocketsHttpHandler.Http2UnencryptedSupport",true);
        HostCheckMode=e.Args.Length==3 && e.Args[0]=="--host-self-test";
        bool captureCheck=e.Args.Length==3 && e.Args[0]=="--capture-self-test";
        bool loaderCheck=e.Args.Length==3 && e.Args[0]=="--verify-loader";
        bool handoffCheck=e.Args.Length==3 && e.Args[0]=="--handoff-self-test";
        bool modeCheck=e.Args.Length==3 && e.Args[0]=="--mode-self-test";
        bool fovCheck=e.Args.Length==3 && e.Args[0]=="--fov-self-test";
        bool endCheck=e.Args.Length==3 && e.Args[0]=="--end-self-test";
        bool fusionFixCheck=e.Args.Length==2 && e.Args[0]=="--fusionfix-self-test";
        bool hookCheck=e.Args.Length==2 && e.Args[0]=="--hook-self-test";
        bool musicCheck=e.Args.Length==2 && e.Args[0]=="--music-self-test";
        bool toolsCheck=e.Args.Length==2 && e.Args[0]=="--lan-tools-self-test";
        bool toolsPreview=e.Args.Length==2 && new[]{"--preview-settings","--preview-recovery","--preview-saves","--preview-players"}.Contains(e.Args[0]);
        PreviewMode=musicCheck || toolsCheck || toolsPreview || captureCheck || hookCheck || fusionFixCheck || fovCheck || endCheck || modeCheck || handoffCheck || loaderCheck || HostCheckMode || (e.Args.Length==2 && (e.Args[0]=="--diagnostics-self-test" || e.Args[0]=="--preview" || e.Args[0]=="--preview-host" || e.Args[0]=="--preview-guide" || e.Args[0]=="--preview-en" || e.Args[0]=="--self-test" || e.Args[0]=="--preview-dialog" || e.Args[0]=="--preview-dialog-long"));
        if(PreviewMode)
        {
            try
            {
            if(captureCheck) {System.IO.File.WriteAllText(System.IO.Path.GetFullPath(e.Args[2]),await Services.PackagedCaptureProbe.RunAsync(e.Args[1],e.Args[2]));Shutdown();return;}
            if(hookCheck) {string report=System.IO.Path.GetFullPath(e.Args[1]); string result=HookDeploymentProbe.Run(System.IO.Path.GetDirectoryName(report)!);System.IO.File.WriteAllText(report,result);Shutdown(result.Contains("FAIL ")?1:0);return;}
            if(fusionFixCheck) {await FusionFixPackageProbe.RunAsync(e.Args[1]);Shutdown();return;}
            if(loaderCheck)
            {
                bool accepted=ScblLan.OriginalLoaderVerifier.IsAuthentic(e.Args[1]);
                System.IO.File.WriteAllText(System.IO.Path.GetFullPath(e.Args[2]),$"AUTHENTICODE_ACCEPTED {accepted}\nSHA256 {ScblLan.GameRuntime.Hash(e.Args[1])}\n");
                Shutdown(accepted?0:2);return;
            }
            var w=new MainWindow();
            if(musicCheck){string result=await w.RunMusicCheckAsync();System.IO.File.WriteAllText(e.Args[1],result);Shutdown(result.Contains("FAIL ")?1:0);return;}
            if(toolsCheck){string result=w.RunLanToolsChecks(e.Args[1]);System.IO.File.WriteAllText(e.Args[1],result);Shutdown(result.Contains("FAIL ")?1:0);return;}
            if(e.Args[0]=="--diagnostics-self-test") {w.ExportDiagnosticFixture(e.Args[1]);Shutdown();return;}
            if(e.Args[0]=="--preview-host") {w.RenderHostPreview(e.Args[1]);Shutdown();return;}
            if(e.Args[0]=="--preview-en")w.SetEnglishPreview();
            if(fovCheck) {System.IO.File.WriteAllText(System.IO.Path.GetFullPath(e.Args[2]),await w.RunFovCheckAsync(e.Args[1],e.Args[2]));Shutdown();return;}
            if(endCheck) {System.IO.File.WriteAllText(System.IO.Path.GetFullPath(e.Args[2]),await w.RunEndCheckAsync(e.Args[1],e.Args[2]));Shutdown();return;}
            if(modeCheck) {System.IO.File.WriteAllText(System.IO.Path.GetFullPath(e.Args[2]),await w.RunModeCheckAsync(e.Args[1],e.Args[2]));Shutdown();return;}
            if(handoffCheck) {System.IO.File.WriteAllText(System.IO.Path.GetFullPath(e.Args[2]),await w.RunHandoffCheckAsync(e.Args[1],e.Args[2]));Shutdown();return;}
            if(HostCheckMode) {System.IO.File.WriteAllText(System.IO.Path.GetFullPath(e.Args[2]),await w.RunHostUiCheckAsync(e.Args[1]));Shutdown();return;}
            if(e.Args[0]=="--self-test") {System.IO.File.WriteAllText(System.IO.Path.GetFullPath(e.Args[1]),w.RunIsolatedChecks());Shutdown();return;}
            if(e.Args[0].StartsWith("--preview-dialog")) w.ShowDiagnosticDialog(e.Args[0].EndsWith("-long"));
            if(toolsPreview)w.ShowToolsPreview(e.Args[0].Replace("--preview-","").Replace("settings","home"));
            var root=(FrameworkElement)w.Content;
            root.Measure(new Size(w.Width,w.Height));root.Arrange(new Rect(0,0,w.Width,w.Height));root.UpdateLayout();
            if(e.Args[0]=="--preview-guide") {w.ShowGuidePreview();root.UpdateLayout();}
            var bmp=new RenderTargetBitmap((int)w.Width,(int)w.Height,96,96,PixelFormats.Pbgra32);bmp.Render(root);
            var encoder=new PngBitmapEncoder();encoder.Frames.Add(BitmapFrame.Create(bmp));
            using(var f=System.IO.File.Create(System.IO.Path.GetFullPath(e.Args[1])))encoder.Save(f);
            Shutdown();return;
            }
            catch(Exception ex)
            {
                System.IO.File.WriteAllText(System.IO.Path.GetFullPath(e.Args[captureCheck || HostCheckMode || loaderCheck || handoffCheck || modeCheck || fovCheck || endCheck ? 2 : 1])+".error.txt",ex.ToString());
                Shutdown(1);return;
            }
        }
        mutex=new Mutex(true,"SCBL_LAN_Refactor_"+Environment.UserName,out owns);
        if(!owns)
        {
            if(Environment.GetEnvironmentVariable("SCBL_THERMAL_MATERIAL_OBSERVER")=="1")
            {
                MessageBox.Show("已有启动器正在运行。请先正常关闭旧启动器，再重新运行热成像诊断入口。",
                    "热成像诊断",MessageBoxButton.OK,MessageBoxImage.Information);
                Shutdown(2);
            }
            else Shutdown();
            return;
        }
        base.OnStartup(e);
        new MainWindow().Show();
    }
    protected override void OnExit(ExitEventArgs e) {if(owns)mutex?.ReleaseMutex();mutex?.Dispose();base.OnExit(e);}
}

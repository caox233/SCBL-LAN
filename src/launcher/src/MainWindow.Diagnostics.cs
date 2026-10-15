using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Windows;
using SplinterCellCNLauncher.Services;
namespace SplinterCellCNLauncher;
public partial class MainWindow
{
    internal void ExportDiagnosticFixture(string output)
    {
        if(!App.PreviewMode)throw new InvalidOperationException();
        txtServer.Text="192.0.2.10:50051";cmbGameplayMode.SelectedValue="MOD";
        DiagnosticBundleService.Export(output,CaptureDiagnosticState(),collectNetwork:false);
    }
    internal void SetEnglishPreview(){if(!App.PreviewMode)throw new InvalidOperationException();_settings.Language="en-US";ApplyLocalization();}
    internal void ShowGuidePreview(){if(!App.PreviewMode)throw new InvalidOperationException();ShowGuide(false);}
    internal void RenderHostPreview(string output)
    {
        if(!App.PreviewMode)throw new InvalidOperationException();
        _=new System.Windows.Interop.WindowInteropHelper(this).EnsureHandle();
        var timer=new System.Windows.Threading.DispatcherTimer{Interval=TimeSpan.FromMilliseconds(30)};
        Exception? error=null;
        timer.Tick+=(_,_)=>{
            var dialog=Application.Current.Windows.OfType<Window>().FirstOrDefault(x=>x.Owner==this);if(dialog==null)return;timer.Stop();
            try {
                var content=(FrameworkElement)dialog.Content;content.UpdateLayout();
                var bmp=new System.Windows.Media.Imaging.RenderTargetBitmap((int)Math.Ceiling(content.ActualWidth),(int)Math.Ceiling(content.ActualHeight),96,96,System.Windows.Media.PixelFormats.Pbgra32);
                bmp.Render(content);var png=new System.Windows.Media.Imaging.PngBitmapEncoder();png.Frames.Add(System.Windows.Media.Imaging.BitmapFrame.Create(bmp));
                using var f=File.Create(output);png.Save(f);
            }catch(Exception e){error=e;}finally{dialog.Close();}
        };
        timer.Start();
        try{ShowHostSelectionDialog(new[]{new DiscoveredHost{HostName="合成测试房主",HostIp="192.0.2.10"}},"当前发现的房主","");}
        finally{timer.Stop();}
        if(error!=null)throw error;
    }
    internal void ShowDiagnosticDialog(bool longMessage)
    {
        if(!App.PreviewMode)throw new InvalidOperationException();
        _=ShowConfirmDialogAsync(longMessage?"连接诊断详情":"结束游戏",longMessage?string.Join("\n",Enumerable.Range(1,40).Select(i=>$"诊断记录 {i}：详细信息可滚动查看，端口保持不变。")):"当前游戏仍在运行。\n\n是否结束游戏？",longMessage?"知道了":"结束游戏","取消");
    }

    internal async Task<string> RunFovCheckAsync(string probe,string output)
    {
        if(!App.PreviewMode)throw new InvalidOperationException();
        var results=new List<string>();
        string dir=Path.Combine(Path.GetDirectoryName(Path.GetFullPath(output))!,"fov-"+Guid.NewGuid().ToString("N"));Directory.CreateDirectory(dir);
        string exe=Path.Combine(dir,"Blacklist_DX11_game.exe");File.Copy(probe,exe);
        using var c=(ScblLan.LanConnection)Activator.CreateInstance(typeof(ScblLan.LanConnection),System.Reflection.BindingFlags.Instance|System.Reflection.BindingFlags.NonPublic,null,new object[]{IPAddress.Loopback,IPAddress.Loopback,50051},null)!;
        string? old=Environment.GetEnvironmentVariable("SCBL_LAN_TEST_FOV");
        try {
            foreach(string mode in new[]{"Official","MOD"}) foreach(double fov in new[]{1.0,1.25,1.3}) {
                cmbGameplayMode.SelectedValue=mode;fovSlider.Value=fov;
                string config=ScblLan.GameRuntime.Config(c,"fixture","synthetic-password","fixture-id",GetSelectedGameplayMode(),GetSelectedFovScale());
                var pipe=new ScblLan.LaunchPipe(config,exe);
                Environment.SetEnvironmentVariable("SCBL_LAN_TEST_FOV",fov.ToString(System.Globalization.CultureInfo.InvariantCulture));
                try {
                    _gameLaunchService.DiagnosticDataRoot=Path.Combine(dir,"diagnostic-data");
                    using var child=await _gameLaunchService.StartGameAsync(dir,"Blacklist_DX11_game.exe",pipe);
                    using var timeout=new CancellationTokenSource(TimeSpan.FromSeconds(15));await child.WaitForExitAsync(timeout.Token);
                    if(child.ExitCode!=0)throw new Exception("Native FOV check failed");
                    results.Add($"PASS UI -> production config -> native parser {GetSelectedGameplayMode()} FOV={fov:0.00}");
                } finally {await _gameLaunchService.StopAsync();await pipe.DisposeAsync();}
            }
        } finally {Environment.SetEnvironmentVariable("SCBL_LAN_TEST_FOV",old);}
        return string.Join('\n',results)+"\nNo real game or saved data accessed.\n";
    }

    internal async Task<string> RunEndCheckAsync(string probe,string output)
    {
        if(!App.PreviewMode)throw new InvalidOperationException();
        string root=Path.Combine(Path.GetDirectoryName(Path.GetFullPath(output))!,"end-"+Guid.NewGuid().ToString("N"));
        string own=Path.Combine(root,"owned"),other=Path.Combine(root,"unrelated");Directory.CreateDirectory(own);Directory.CreateDirectory(other);
        string name=Path.GetFileName(probe);File.Copy(probe,Path.Combine(own,name));File.Copy(probe,Path.Combine(other,name));
        _gameDir=own;_launchStartedUtc=DateTime.UtcNow.AddSeconds(-1);_runningGameProcessName=Path.GetFileNameWithoutExtension(name);
        System.Diagnostics.Process Start(string dir)=>System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(Path.Combine(dir,name)){WorkingDirectory=dir,UseShellExecute=false,CreateNoWindow=true,WindowStyle=System.Diagnostics.ProcessWindowStyle.Hidden})!;
        using var owned=Start(own);using var unrelated=Start(other);
        string? failure=null;var observer=new System.Windows.Threading.DispatcherTimer{Interval=TimeSpan.FromMilliseconds(25)};
        observer.Tick+=(_,_)=>{if(dialogOverlay.Visibility==Visibility.Visible){failure=txtDialogMessage.Text;DialogYesButton_Click(this,new RoutedEventArgs());}};
        observer.Start();
        try {
            await Task.Delay(350);SetGameRunningState(true);
            await EndRunningGameAsync("isolated owned-process fixture");
            if(!owned.HasExited || unrelated.HasExited || failure!=null || _isGameRunning || _isEndingGame)throw new Exception("End lifecycle check failed: "+failure);
            await EndRunningGameAsync("repeated stop is idempotent");
            if(unrelated.HasExited || failure!=null)throw new Exception("Repeated stop affected unrelated process");
            return "PASS owned synthetic process exited\nPASS unrelated same-name process remains alive\nPASS no false failure dialog; flags returned to idle\nPASS repeated stop is harmless\nNo real game was terminated.\n";
        } finally {
            observer.Stop();
            foreach(var p in new[]{owned,unrelated}) {if(!p.HasExited){p.Kill(false);await p.WaitForExitAsync();}}
        }
    }
    [System.Runtime.InteropServices.UnmanagedFunctionPointer(System.Runtime.InteropServices.CallingConvention.StdCall)]
    private delegate int PluginInitializeProbe(IntPtr host,IntPtr config);
    internal async Task<string> RunModeCheckAsync(string probe,string output)
    {
        if(!App.PreviewMode)throw new InvalidOperationException("Isolated mode only");
        string dir=Path.Combine(Path.GetDirectoryName(Path.GetFullPath(output))!,"app-modes-"+Guid.NewGuid().ToString("N"));Directory.CreateDirectory(dir);
        string exe=Path.Combine(dir,"Blacklist_DX11_game.exe");File.Copy(probe,exe);var result=new List<string>();
        // Load the fresh x86 plugin and exercise only its null-input ABI rejection, never game patches.
        string plugin=Path.Combine(SplinterCellCNLauncher.Services.GameplayRuntime.ValidateForLaunch("MOD",SplinterCellCNLauncher.Services.RuntimePaths.Root)!,"scbl_gameplay.dll");
        IntPtr module=System.Runtime.InteropServices.NativeLibrary.Load(plugin);
        try {
            var fn=System.Runtime.InteropServices.Marshal.GetDelegateForFunctionPointer<PluginInitializeProbe>(System.Runtime.InteropServices.NativeLibrary.GetExport(module,"scbl_plugin_initialize"));
            if(fn(IntPtr.Zero,IntPtr.Zero)!=-1)throw new Exception("Unexpected native plugin ABI result");
            result.Add("PASS packaged x86 gameplay DLL loads and rejects null ABI input");
        } finally{System.Runtime.InteropServices.NativeLibrary.Free(module);}
        foreach(string requested in new[]{"Official","MOD"}) {
            cmbGameplayMode.SelectedValue=requested;string mode=GetSelectedGameplayMode();
            string config=$"ConfigServer = \"127.0.0.1\"\nApiServer = \"http://127.0.0.1:50051/\"\nGameplayMode = \"{mode}\"\n[User]\nUsername = \"fixture\"\nPassword = \"synthetic-test-password\"\nAccountId = \"fixture-id\"\n";
            var pipe=new ScblLan.LaunchPipe(config,exe);
            try {
                string? pluginDir=SplinterCellCNLauncher.Services.GameplayRuntime.ValidateForLaunch(mode,SplinterCellCNLauncher.Services.RuntimePaths.Root);
                _gameLaunchService.DiagnosticDataRoot=Path.Combine(dir,"diagnostic-data");
                    using var child=await _gameLaunchService.StartGameAsync(dir,"Blacklist_DX11_game.exe",pipe,pluginDirectory:pluginDir);
                using var timeout=new CancellationTokenSource(TimeSpan.FromSeconds(20));await child.WaitForExitAsync(timeout.Token);
                if(child.ExitCode!=0)throw new Exception("Packaged native mode check failed: "+mode);
                result.Add("PASS actual UI selection -> native config process: "+mode);
            } finally {await _gameLaunchService.StopAsync();await pipe.DisposeAsync();}
        }
        return string.Join('\n',result)+"\nNo real game or saved data accessed.\n";
    }

    internal async Task<string> RunHandoffCheckAsync(string probe,string output)
    {
        if(!App.PreviewMode) throw new InvalidOperationException("Requires isolated diagnostic mode");
        string dir=Path.Combine(Path.GetDirectoryName(Path.GetFullPath(output))!,"app-handoff-"+Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);string exe=Path.Combine(dir,"Blacklist_DX11_game.exe");File.Copy(probe,exe);
        const string config="ConfigServer = \"127.0.0.1\"\nApiServer = \"http://127.0.0.1:50051/\"\nGameplayMode = \"Official\"\n[User]\nUsername = \"fixture\"\nPassword = \"synthetic-test-password\"\nAccountId = \"fixture-id\"\n";
        var pipe=new ScblLan.LaunchPipe(config,exe);
        try {
            using var timeout=new CancellationTokenSource(TimeSpan.FromSeconds(20));
            _gameLaunchService.DiagnosticDataRoot=Path.Combine(dir,"diagnostic-data");
            using var first=await _gameLaunchService.StartGameAsync(dir,"Blacklist_DX11_game.exe",pipe,timeout.Token);
            await first.WaitForExitAsync(timeout.Token);
            if(first.ExitCode!=0 || !File.Exists(Path.Combine(dir,"scbl-lan.toml")))throw new Exception("First stage lost configuration session");
            using var second=System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(exe){WorkingDirectory=dir,UseShellExecute=false,CreateNoWindow=true})!;
            await second.WaitForExitAsync(timeout.Token);
            if(second.ExitCode!=0)throw new Exception("Second native configuration process failed");
        } finally {await _gameLaunchService.StopAsync();await pipe.DisposeAsync();}
        if(File.Exists(Path.Combine(dir,"scbl-lan.toml")))throw new Exception("Session reference was not cleaned");
        return "PASS packaged x86 launcher -> first native config process\nPASS first process exit preserves handoff session\nPASS second native config process\nPASS owned reference removed on session end\nNo real game or player data accessed.\n";
    }

    internal async Task<string> RunHostUiCheckAsync(string ip)
    {
        if (!App.HostCheckMode) throw new InvalidOperationException("Requires isolated host test mode");
        _settings.BindOverride=ip;_isHostModeCached=true;
        txtUsername.Text="synthetic_ui_host";
        PopulateAdapters();
        // Exercise the real UI command and failure dialog, not just a separate socket helper.
        string? failure=null;
        var observer=new System.Windows.Threading.DispatcherTimer {Interval=TimeSpan.FromMilliseconds(50)};
        observer.Tick+=(_,_)=> {if(dialogOverlay.Visibility==Visibility.Visible){failure=txtDialogMessage.Text;DialogYesButton_Click(this,new RoutedEventArgs());}};
        observer.Start();
        Exception? operationFailure = null;
        try
        {
            bool ok=await StartHostModeServerFromUiAsync();
            if(!ok)throw new IOException("Actual host UI command failed: "+failure);
            string endpoint=$"{ip}:{_hostRuntime.ApiPort}";
            if(txtServer.Text!=endpoint) throw new Exception("Host UI lost the selected port");
            using var c=await ScblLan.LanConnection.ConnectAsync(endpoint,ip,CancellationToken.None);
            if(!c.Capabilities.Ready)throw new Exception("Host UI marked ready before services were ready");
            return $"PASS actual StartHostModeServerFromUiAsync\nHOST_READY {endpoint}\nPASS server textbox retains actual API port\nPASS real capabilities ready\nPASS owned process stopped in finally\n";
        }
        catch (Exception ex) { operationFailure = ex; throw; }
        finally
        {
            observer.Stop();
            if (!StopDedicatedServer("isolated UI test complete"))
                throw new IOException("Owned host process exit was not confirmed during isolated UI cleanup; retry stopping it.", operationFailure);
        }
    }

    internal string RunIsolatedChecks()
    {
        var results=new List<string>();
        void Check(bool b,string label) {if(!b)throw new Exception(label);results.Add("PASS "+label);}
        foreach(string password in new[]{"1","a","密"," ","Ａ","🔒","  密码  ","'\"\\",new string('中',50)})
        {
            Check(IsValidPasswordInput(password),"password input has no strength or character-class requirement");
            txtPassword.Password=password;Check(txtPassword.Password==password,"PasswordBox preserves exact synthetic password");
        }
        Check(!PasswordPolicy.IsValidPassword(""),"empty password still rejected");
        Check(PasswordPolicy.IsValidForNativeGame(new string('a',63)),"native ASCII63 accepted");
        Check(!PasswordPolicy.IsValidForNativeGame(new string('a',64)),"native ASCII64 rejected without truncation");
        Check(PasswordPolicy.IsValidForNativeGame(new string('中',21)),"native UTF8 63-byte boundary accepted");
        Check(!PasswordPolicy.IsValidForNativeGame(new string('中',22)),"native UTF8 overflow rejected by bytes not characters");
        Check(PasswordPolicy.IsValidForNativeGame(" ") && PasswordPolicy.IsValidForNativeGame("🔒"),"native short space and Unicode passwords preserved");
        Check(!PasswordPolicy.IsValidForNativeGame("") && !PasswordPolicy.IsValidForNativeGame("\0") && !PasswordPolicy.IsValidForNativeGame("\ud800"),"native empty NUL and invalid UTF16 rejected");
        Check(PasswordPolicy.IsValidPassword(new string('a',4096)),"account API password capacity unchanged");

        Check(!IsValidPasswordInput("\0") && !IsValidPasswordInput(new string('x',4097)),"native encoding and capacity guard retained");
        txtPassword.Password="";
        Check(FindName("btnExportDiagnostics")==btnExportDiagnostics && btnExportDiagnostics.IsEnabled,"export diagnostics replaces old Hook slot and is enabled");
        foreach(string name in new[]{"txtAppSubtitle","txtFooterNotice","txtLaunchModeLabel","cmbGameExecutable","txtHostDiscovery","btnRestoreHook"})Check(FindName(name)==null,"removed obsolete UI "+name);
        Check(((System.Windows.Controls.ListBoxItem)cmbGameplayMode.Items[0]).Tag?.ToString()=="MOD" && ((System.Windows.Controls.ListBoxItem)cmbGameplayMode.Items[1]).Tag?.ToString()=="Official","MOD above Official; stable semantic tags");
        Check(chkRememberPassword.IsChecked==true,"remember-password choice visible and enabled by default");
        txtUsername.Text="fixture";txtServer.Text="192.0.2.10";
        _restoringPassword=true;txtPassword.Password="synthetic-only";_restoringPassword=false;
        _autoPasswordScope=SplinterCellCNLauncher.Services.CredentialStore.Scope(txtServer.Text,txtUsername.Text);
        txtServer.Text="192.0.2.11";Check(txtPassword.Password.Length==0,"switching server clears auto-filled password before login");
        _restoringPassword=true;txtPassword.Password="synthetic-only";_restoringPassword=false;
        _autoPasswordScope=SplinterCellCNLauncher.Services.CredentialStore.Scope(txtServer.Text,txtUsername.Text);
        txtUsername.Text="fixture-other";Check(txtPassword.Password.Length==0,"switching account clears auto-filled password before login");
        txtPassword.Password="synthetic-manual";txtServer.Text="192.0.2.12";
        Check(txtPassword.Password=="synthetic-manual","manual password is not silently overwritten by auto-fill");
        txtPassword.Password="";txtUsername.Text="";txtServer.Text="";
        string captured=SplinterCellCNLauncher.Services.CredentialStore.Scope("192.0.2.10","fixture");
        Check(CanUseCapturedPassword(captured,"192.0.2.10:50051","fixture"),"canonical endpoint retains captured autofill identity");
        Check(!CanUseCapturedPassword(captured,"192.0.2.11","fixture"),"async host endpoint change rejects captured autofill before authentication");
        Check(!CanUseCapturedPassword(captured,"192.0.2.10","another"),"captured autofill cannot move to another account");
        Check(CanUseCapturedPassword(null,"192.0.2.11","fixture"),"explicitly typed password remains user-controlled");
        _settings.Language=null!;ApplyLocalization();Check(!IsEnglish,"null language preference falls back without startup exception");_settings.Language="zh-CN";
        _isGameRunning=true;Check(!CanCloseAfterGameStop(),"failed game stop cannot close launcher and host");
        _isGameRunning=false;_isEndingGame=true;Check(!CanCloseAfterGameStop(),"in-progress game stop cannot close launcher and host");
        _isEndingGame=false;_isGameStarting=true;Check(!CanCloseAfterGameStop(),"pending startup cannot be treated as a completed stop");
        _isGameStarting=false;Check(CanCloseAfterGameStop(),"completed game stop allows explicit close");
        Check(Math.Abs(GetSelectedFovScale()-1.2)<0.00001,"FOV defaults to SCBL 1.20x");
        fovSlider.Value=1.25;Check(txtFovValue.Text=="1.25×" && GetSelectedFovScale()==1.25,"FOV slider value and label agree");
        Check(SplinterCellCNLauncher.Services.GameplayRuntime.NormalizeFov(double.NaN)==1.2 && SplinterCellCNLauncher.Services.GameplayRuntime.NormalizeFov(9)==1.3 && SplinterCellCNLauncher.Services.GameplayRuntime.NormalizeFov(-2)==1,"FOV invalid/boundary values normalized");
        var persisted=System.Text.Json.JsonSerializer.Deserialize<SplinterCellCNLauncher.Models.LauncherSettings>(System.Text.Json.JsonSerializer.Serialize(new SplinterCellCNLauncher.Models.LauncherSettings{FovScale=1.25}));
        Check(persisted?.FovScale==1.25,"FOV setting serialization round trip");
        FovReset_Click(this,new RoutedEventArgs());Check(GetSelectedFovScale()==1.2,"FOV reset restores default");
        Check(GetSelectedGameplayMode()=="MOD","gameplay mode defaults to MOD");
        Check(GetSelectedGameExecutable()=="Blacklist_DX11_game.exe","DX11 remains fixed without a renderer selector");
        cmbGameplayMode.SelectedValue="MOD";
        Check(GetSelectedGameplayMode()=="MOD" && GetSelectedGameExecutable()=="Blacklist_DX11_game.exe","MOD is independent of renderer choice");
        Check(FindName("cmbGameExecutable")==null && GetSelectedGameExecutable()=="Blacklist_DX11_game.exe" && GetSelectedGameplayMode()=="MOD","renderer selector removed; DX11-only and MOD retained");
        SetBusy(true);Check(!cmbGameplayMode.IsEnabled,"gameplay selection locked while busy");SetBusy(false);
        SetGameStartingState(true);Check(!cmbGameplayMode.IsEnabled,"gameplay selection locked while starting");
        Check(!fovSlider.IsEnabled && !btnFovReset.IsEnabled,"FOV controls locked while starting");
        SetGameRunningState(true);Check(!cmbGameplayMode.IsEnabled,"gameplay selection locked while running");
        Check(!fovSlider.IsEnabled,"FOV controls locked while running");
        SetGameRunningState(false);Check(cmbGameplayMode.IsEnabled,"gameplay selection restored after exit");
        Check(fovSlider.IsEnabled,"FOV controls restored after exit");
        cmbGameplayMode.SelectedValue="Official";
        string session=Guid.NewGuid().ToString("N");
        string packet=BuildHostBroadcastMessage("fixture","192.0.2.10",session);
        var host=ParseHostBroadcastMessage(packet);
        Check(host?.HostIp=="192.0.2.10" && host.ApiPort==50051 && host.SessionId==session,"discovery round trip accepts non-26 address");
        var advertised = ParseHostBroadcastMessage(BuildHostBroadcastMessage("fixture", "192.0.2.10", session, 50051));
        Check(advertised?.Endpoint == "192.0.2.10:50051", "discovery preserves fixed API port in join endpoint");
        Check(ScblLan.ServerAddress.Parse(advertised!.Endpoint).Port == 50051, "manual address parser retains fixed advertised port");
        Check(ParseHostBroadcastMessage(packet.Replace("SCBL_LAN_HOST","SCBL_CN_HOST"))==null,"old-product discovery excluded");
        Check(ParseHostBroadcastMessage(packet.Replace("ApiPort=50051","ApiPort=70000"))==null,"discovery rejects invalid port");
        Check(ParseHostBroadcastMessage(packet.Replace("Version=1","Version=2"))==null,"discovery rejects unknown version");
        Check(ParseHostBroadcastMessage(packet.Replace("State=ServerReady","State=Starting"))==null,"discovery rejects nonready server");
        Check(ParseHostBroadcastMessage(packet.Replace("HostName=fixture","HostName="+new string('x',65)))==null,"discovery name bounded");
        Check(IsHostDiscoveryRequestMessage(BuildHostDiscoveryRequestMessage("192.0.2.11")),"query round trip");
        Check(IsValidServerAddressText("example.test:50051",out _),"manual hostname allowed without heartbeat");
        Check(!IsValidServerAddressText("http://name:pass@example.test",out _),"embedded credentials rejected");
        Check(!IsValidServerAddressText("https://example.test",out _),"unsupported scheme rejected");
        _isBusy=false;_isGameRunning=false;_isGameStarting=false;
        UpdateServerAddressInputState(false);
        Check(txtServer.IsEnabled,"manual server editable without ready adapter");
        _isGameRunning=true;UpdateServerAddressInputState(false);
        Check(!txtServer.IsEnabled,"server edit disabled during game");
        _isGameRunning=false;_isGameStarting=true;UpdateServerAddressInputState(false);
        Check(!txtServer.IsEnabled,"server edit disabled during startup");
        _isGameStarting=false;UpdateServerAddressInputState(false);
        Check(txtServer.IsEnabled,"server edit restored after game");
        Check(EnsureCurrentHostHeartbeatOrFailAsync("192.0.2.10",false).GetAwaiter().GetResult(),"heartbeat does not gate manual connection");
        // Real local UDP sockets; this proves packet round trip only, not remote gameplay connectivity.
        using var a=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));
        using var b=new UdpClient(new IPEndPoint(IPAddress.Loopback,0));
        a.Client.ReceiveTimeout=b.Client.ReceiveTimeout=2000;
        var toB=(IPEndPoint)b.Client.LocalEndPoint!;var remote=new IPEndPoint(IPAddress.Any,0);
        var bytes=Encoding.UTF8.GetBytes(packet);a.Send(bytes,bytes.Length,toB);
        var received=b.Receive(ref remote);Check(ParseHostBroadcastMessage(Encoding.UTF8.GetString(received))!=null,"actual UDP discovery packet decoded");
        b.Send(received,received.Length,remote);a.Receive(ref remote);Check(true,"actual loopback UDP reply received");
        foreach(var size in new[] { new Size(1120,680), new Size(1000,600), new Size(900,580) })
        {
            rootGrid.Measure(size); rootGrid.Arrange(new Rect(new Point(),size)); rootGrid.UpdateLayout();
            Rect Bounds(FrameworkElement e) => new(e.TranslatePoint(new Point(), rootGrid), new Size(e.ActualWidth,e.ActualHeight));
            var left=Bounds(heroPanel);var right=Bounds(authPanel);
            Check(left.Right < right.Left && Bounds(btnLaunch).Left < size.Width/2,
                $"SCBL left launch / right login layout {size.Width}x{size.Height}");
            Check(Bounds(txtUsername).Left > size.Width/2 && Bounds(txtUsername).Top < Bounds(txtPassword).Top &&
                  Bounds(txtPassword).Top < Bounds(txtServer).Top, $"SCBL vertical sign-in fields {size.Width}x{size.Height}");
            Check(Bounds(btnExportDiagnostics).Right<Bounds(btnCheckNetwork).Left,"diagnostic export occupies former Hook slot before network check");
            foreach(var e in new FrameworkElement[]{btnLaunch,txtUsername,txtPassword,txtServer,cmbBindAdapter,btnExportDiagnostics,btnCheckNetwork,btnWindowClose,cmbGameplayMode,fovSlider,chkRememberPassword})
            {
                var rect=Bounds(e);
                Check(rect.Width>10 && rect.Height>10 && rect.Left>=0 && rect.Top>=0 && rect.Right<=size.Width+1 && rect.Bottom<=size.Height+1,
                    $"visible control {e.Name} {size.Width}x{size.Height}");
            }
            ShowDiagnosticDialog(true);rootGrid.Measure(size);rootGrid.Arrange(new Rect(new Point(),size));rootGrid.UpdateLayout();
            var card=Bounds(dialogCard);
            Check(dialogCard.CornerRadius.TopLeft==0 && card.Left>=0 && card.Right<=size.Width && card.Top>=0 && card.Bottom<=size.Height,$"SCBL square dialog stays within {size.Width}x{size.Height}");
            Check(dialogMessageScroll.ScrollableHeight>0,$"long diagnostic message scrolls {size.Width}x{size.Height}");
            DialogNoButton_Click(this,new RoutedEventArgs());Check(dialogOverlay.Visibility==Visibility.Collapsed,"dialog cancel closes overlay");
        }
        // Exercise the real modal picker without showing it or using any discovered player host.
        _=new System.Windows.Interop.WindowInteropHelper(this).EnsureHandle();
        var fixtureHosts=new[]{new DiscoveredHost{HostName="fixture-a",HostIp="192.0.2.20"},new DiscoveredHost{HostName="fixture-b",HostIp="192.0.2.21"}};
        foreach(string action in new[]{"window-close","cancel","choose","double-click"})
        {
            var timer=new System.Windows.Threading.DispatcherTimer{Interval=TimeSpan.FromMilliseconds(20)};
            Exception? actionFailure=null;
            IEnumerable<System.Windows.Controls.Button> Buttons(System.Windows.DependencyObject parent)
            {
                for(int i=0;i<System.Windows.Media.VisualTreeHelper.GetChildrenCount(parent);i++) {
                    var child=System.Windows.Media.VisualTreeHelper.GetChild(parent,i);
                    if(child is System.Windows.Controls.Button b)yield return b;
                    foreach(var item in Buttons(child))yield return item;
                }
            }
            timer.Tick+=(_,_)=>{
                var dialog=Application.Current.Windows.OfType<Window>().FirstOrDefault(x=>x.Owner==this);
                if(dialog==null)return;
                timer.Stop();
                try {
                    if(action=="window-close") {dialog.Close();return;}
                    var buttons=Buttons(dialog).ToArray();
                    if(action=="cancel")buttons.Single(b=>Equals(b.Content,"取消")).RaiseEvent(new RoutedEventArgs(System.Windows.Controls.Button.ClickEvent));
                    else {
                        var row=buttons.Where(b=>b.Content is System.Windows.Controls.Grid).Last();
                        row.RaiseEvent(new RoutedEventArgs(System.Windows.Controls.Button.ClickEvent));
                        if(action=="double-click")row.RaiseEvent(new System.Windows.Input.MouseButtonEventArgs(System.Windows.Input.Mouse.PrimaryDevice,0,System.Windows.Input.MouseButton.Left){RoutedEvent=System.Windows.Controls.Control.MouseDoubleClickEvent});
                        else buttons.Single(b=>Equals(b.Content,"连接房主")).RaiseEvent(new RoutedEventArgs(System.Windows.Controls.Button.ClickEvent));
                    }
                } catch(Exception e){actionFailure=e;dialog.Close();}
            };
            timer.Start();
            DiscoveredHost? selected;
            try {selected=ShowHostSelectionDialog(fixtureHosts,"合成房主选择测试","");}
            finally {timer.Stop();}
            if(actionFailure!=null)throw actionFailure;
            Check(action is "window-close" or "cancel" ? selected==null : ReferenceEquals(selected,fixtureHosts[1]),"host picker "+action+" requires explicit confirmation");
            Check(!_isHostSelectionDialogOpen,"host picker "+action+" resets modal guard");
        }
        return string.Join(Environment.NewLine,results)+$"\nRESULT {results.Count} isolated UI/protocol checks passed.\n";
    }
}

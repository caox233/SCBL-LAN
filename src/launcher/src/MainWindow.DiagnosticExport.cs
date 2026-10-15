using Microsoft.Win32;
using System.Windows;
using SplinterCellCNLauncher.Services;

namespace SplinterCellCNLauncher;
public partial class MainWindow
{
    private async void CaptureOnce_Click(object sender,RoutedEventArgs e)
    {
        if(App.PreviewMode || _isBusy || _isGameStarting || _isGameRunning)return;
        var consent=await ShowConfirmDialogAsync(L("诊断启动一次", "Capture one diagnostic launch"),
            L("仅为这次启动启用进程外异常采集（最多10分钟）。调试会影响游戏时序；首次已处理异常不会收集。\n\n若发生未处理异常，保存一次最多16 MiB的最小转储、寄存器和调用栈。即使最小转储也可能含敏感栈数据。不会自动上传，不加入普通诊断包；分享时仍需单独确认。现场保留最多24小时。\n\n是否同意并启动？", "Enable out-of-process exception capture for this launch only (up to 10 minutes). Debugging may change timing; handled first-chance exceptions are not captured.\n\nOne unhandled exception can produce a minimal dump up to 16 MiB, registers and a call stack. It may contain sensitive stack data. No upload or inclusion in ordinary diagnostics; sharing requires separate consent. Retained for up to 24 hours.\n\nConsent and launch?"), L("同意并启动","Consent and launch"),L("取消","Cancel"));
        if(consent!=MessageBoxResult.Yes)return;
        if(_isBusy || _isGameStarting || _isGameRunning)return;
        if(!CanLaunchGame()){await RefreshServerStatusAsync(keepButtonDisabled:true,showFailureDialog:true);if(!CanLaunchGame())return;}
        _gameLaunchService.CaptureNextLaunch=true;
        try {await RunLaunchFlowAsync();}
        finally {_gameLaunchService.CaptureNextLaunch=false;}
    }
    private async void ExportCapture_Click(object sender,RoutedEventArgs e)
    {
        if(App.PreviewMode||_isExportingDiagnostics)return;
        var current=RunDiagnosticStore.Current;
        if(current==null){await ShowInfoDialogAsync(L("暂无异常现场","No exception capture"),L("请先使用“诊断启动一次”复现。普通启动不生成转储。","Use a diagnostic launch to reproduce first. Normal launches do not create dumps."));return;}
        if(await ShowConfirmDialogAsync(L("单独导出异常现场","Export private exception capture"),L("该文件可能包含敏感栈数据，不是保证脱敏的诊断包。请只分享给可信的分析人员；没有自动上传。是否继续？","This may contain sensitive stack data and is not guaranteed anonymized. Share only with trusted analysts. Nothing is uploaded. Continue?"))!=MessageBoxResult.Yes)return;
        var dialog=new SaveFileDialog {Title=L("导出异常现场","Export exception capture"),Filter="ZIP (*.zip)|*.zip",DefaultExt=".zip",AddExtension=true,OverwritePrompt=false,FileName="SCBL-LAN-PrivateCapture-"+DateTime.Now.ToString("yyyyMMdd-HHmmss")+".zip"};
        if(dialog.ShowDialog(this)!=true)return;
        _isExportingDiagnostics=true;
        try {await Task.Run(()=>NativeCaptureSession.ExportPrivate(current.CaptureDirectory,dialog.FileName));await ShowInfoDialogAsync(L("已导出","Exported"),dialog.FileName);}
        catch(Exception ex){await ShowInfoDialogAsync(L("没有可导出的异常现场","Capture unavailable"),L("本次未取得完整异常现场、文件已过期或目标文件已存在。请查看普通诊断包中的采集状态；不会把未捕获当作成功。错误类型：","Capture is incomplete, expired, or the destination already exists. Check capture status in ordinary diagnostics. Error: ")+ex.GetType().Name);}
        finally {_isExportingDiagnostics=false;}
    }
    private bool _isExportingDiagnostics;
    private DiagnosticState CaptureDiagnosticState() => new() {
        HostMode=IsHostMode(),GameplayMode=GetSelectedGameplayMode(),Fov=GetSelectedFovScale(),
        ServerAddress=txtServer.Text.Trim(),BindAddress=_settings.BindOverride,HostBindAddress=_hostBindIp,ServerStatus=(int)_serverStatusKind,
        ServerReachable=_lastServerReachable,GameStarting=_isGameStarting,GameRunning=_isGameRunning,
        HostRunning=!App.PreviewMode && IsDedicatedServerRunning()
    };
    private async void ExportDiagnostics_Click(object sender,RoutedEventArgs e)
    {
        if(_isExportingDiagnostics || App.PreviewMode)return;
        if(await ShowConfirmDialogAsync(L("导出日志与脱敏配置", "Export logs and redacted configs"), L("此诊断包包含已审生产者的现存日志全文，仅按已知规则脱敏，仍可能含敏感信息。请在分享前检查；不会自动上传。已持久化配置按schema脱敏；历史有效配置快照尚未实施，缺失/范围见清单。是否继续？", "This bundle includes full retained text from reviewed producers. Known-rule minimization cannot guarantee removal of every secret. Review before sharing; nothing is uploaded. Persisted configuration schemas are redacted; historical effective snapshots are not implemented. See the manifest for missing sources and scope. Continue?"))!=MessageBoxResult.Yes)return;
        if(_isExportingDiagnostics)return;
        var dialog=new SaveFileDialog {
            Title=L("导出诊断包","Export diagnostics"),Filter="ZIP (*.zip)|*.zip",DefaultExt=".zip",AddExtension=true,OverwritePrompt=true,
            FileName="SCBL-LAN-Diagnostics-"+DateTime.Now.ToString("yyyyMMdd-HHmmss")+".zip"
        };
        if(dialog.ShowDialog(this)!=true)return;
        _isExportingDiagnostics=true;btnExportDiagnostics.IsEnabled=false;
        try {
            var snapshot=CaptureDiagnosticState();string destination=dialog.FileName;string? gameRoot=string.IsNullOrEmpty(_gameDir)?null:_gameDir;
            string output=await Task.Run(()=>DiagnosticBundleService.ExportRetained(destination,snapshot,overwrite:true,gameRoot:gameRoot));
            await ShowInfoDialogAsync(L("诊断包已导出（范围未完成）","Diagnostics exported (scope incomplete)"),L("保存到：\n","Saved to:\n")+output+L("\n\n包含现存日志全文、经过字段校验的运行记录、持久配置的字段脱敏角色及逐文件清单。历史有效配置快照尚未实施；请查看 manifest 的缺失、变化和范围。按已知规则脱敏仍可能保留敏感信息，分享前请检查；不会自动上传。只读取现存快照，不为导出刷新或清理源；数据库、凭据、转储和存档不读取。","\n\nIncludes retained full text, schema-validated run records, typed redacted persisted configuration roles and a per-file manifest. Historical effective snapshots are not implemented; see missing, changed and scope entries. Known-rule minimization may leave sensitive data; review before sharing. No upload. Existing snapshots are read without export-time flush or cleanup; databases, credentials, dumps and saves are not read."));
        } catch(Exception ex) {
            await ShowInfoDialogAsync(L("导出失败","Export failed"),L("未能写入诊断包，请检查目标位置和写入权限。错误类型：","Could not write the diagnostic bundle. Check the destination and write permission. Error type: ")+ex.GetType().Name);
        } finally {_isExportingDiagnostics=false;btnExportDiagnostics.IsEnabled=true;}
    }
}

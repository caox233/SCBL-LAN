using SplinterCellCNLauncher.Services;

namespace SplinterCellCNLauncher;

public partial class MainWindow
{
    private async Task PrepareFusionFixForLaunchAsync(string gameDir, string gameplayMode)
    {
        SetBusy(true, L("准备 FusionFix...", "Preparing FusionFix..."));
        try
        {
            await new FusionFixDeploymentService().EnsureForLaunchAsync(gameDir, gameplayMode);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or AggregateException)
        {
            LogService.Error("FusionFix preparation failed: " + ex.GetType().Name);
            throw new IOException(L(
                "FusionFix 离线部署失败，已取消启动。请保留现有 MOD 和配置，检查游戏目录写入权限或文件占用后重试；不要删除存档。若仍失败，请导出诊断包。",
                "Offline FusionFix preparation failed; launch was cancelled. Keep existing MODs and configuration. Check write access and file locks, then retry. Do not delete saves. Export diagnostics if the issue persists."), ex);
        }
    }
}

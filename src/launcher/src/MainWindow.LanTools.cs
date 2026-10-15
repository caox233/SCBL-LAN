using System.Net;
using System.Net.Sockets;
using System.Windows;
using System.Windows.Input;
using System.Windows.Threading;
using ScblLan;
using SplinterCellCNLauncher.Services;

namespace SplinterCellCNLauncher;

public partial class MainWindow
{
    private readonly PasswordRecoveryService _passwordRecovery = new();
    private readonly LanPresenceRegistry _presenceRegistry = new();
    private readonly LanPresenceClient _presenceClient = new();
    private readonly CancellationTokenSource _toolsCts = new();
    private DispatcherTimer? _presenceTimer;
    private bool _presencePending, _toolsOperation;
    private Task _presenceTask = Task.CompletedTask;
    private string _recoveryServer = "", _recoveryUser = "", _recoveryBind = "";
    private string _toolsPage = "home";
    private volatile string _presenceMode = "MOD";
    private volatile bool _presenceInGame;
    private LanPresenceSnapshot? _lastPresence;

    private void StartLanTools()
    {
        _presenceTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(3) };
        _presenceTimer.Tick += PresenceTimer_Tick; _presenceTimer.Start();
    }
    private async void PresenceTimer_Tick(object? sender, EventArgs e) => await RefreshPresenceAsync();
    private void StopLanTools()
    {
        if (_presenceTimer != null) { _presenceTimer.Stop(); _presenceTimer.Tick -= PresenceTimer_Tick; }
        _toolsCts.Cancel(); ClearRecoveryFields();
    }
    private void ApplyToolsLocalization()
    {
        btnSettings.Content = L("设置", "Settings");
        txtToolsTitle.Text = L("设置", "Settings");
        btnForgotPassword.Content = btnRecoveryPage.Content = L("忘记密码", "Forgot password");
        btnSavesPage.Content = L("恢复完整存档", "Restore saves");
        btnPlayersPage.Content = L("在线玩家 / 联机质量", "Players / quality");
        btnAdvancedDiagnosticsPage.Content = L("高级诊断", "Advanced diagnostics");
        btnCaptureOnce.Content = L("诊断启动一次", "Diagnostic launch");
        btnExportCapture.Content = L("导出异常现场", "Export private capture");
        txtAdvancedDiagnosticsInfo.Text = L("仅按明确同意采集一次未处理异常；最小转储可能含敏感栈数据，不会自动上传，也不进入普通诊断包。", "One unhandled exception is captured only with explicit consent. A minimal dump may contain sensitive stack data; it is not uploaded or included in ordinary diagnostics.");
        txtToolsIntro.Text = L("选择上方功能。存档按官方和 MOD 分开恢复；不会在正常启动时自动覆盖。", "Choose a tool above. Official and MOD saves are restored separately, never automatically on launch.");
        txtRecoveryHelp.Text = L("使用注册时保存的恢复码重置。成功后旧恢复码失效，请保存新码；不会自动保存或记录恢复码。没有恢复码时请联系房主。", "Use the recovery code saved at registration. Success consumes it and issues a new code: save it yourself. Codes are not logged or stored. Without a code, contact the host.");
        lblRecoveryCode.Text = L("注册时的恢复码", "Recovery code");
        lblNewPassword.Text = L("新密码（至少一个字符）", "New password (at least one character)");
        lblConfirmPassword.Text = L("再次输入新密码", "Confirm new password");
        btnResetPassword.Content = L("重置密码", "Reset password");
        txtSavesInfo.Text = L("使用本机 2026-10-04 的完整磁盘快照替换对应模式的两个存档槽。MOD 包含独立配装文件，不能与官方混用。覆盖前自动备份；请先退出游戏。快照的解锁进度尚未经游戏内核验。", "Replace both save slots using the local 2026-10-04 disk snapshot. MOD includes its separate loadout file and cannot be mixed with Official. Existing saves are backed up first. Exit the game before restoring. Unlock progress has not been verified in-game.");
        txtSavePaths.Text = "Official: " + _saveGameService.DirectoryFor("Official") + "\nMOD: " + _saveGameService.DirectoryFor("MOD");
        btnRestoreOfficial.Content = L("恢复官方存档", "Restore Official");
        btnRestoreMod.Content = L("恢复 MOD 存档", "Restore MOD");
        btnRefreshPlayers.Content = L("刷新", "Refresh");
        txtPlayersHelp.Text = L("同一房主下的新版本启动器在线列表（名称为用户填报，不是游戏好友或房间成员名单）。延迟、抖动、丢包为到房主的 UDP 应用探针，不代表游戏端口全部可用。双方都需更新启动器。", "Updated launchers connected to this host (self-reported names, not game friends or lobby members). RTT, jitter and loss measure an application UDP probe to the host, not every game port. Update both launchers.");
        RenderPresence();
    }
    private void ClearRecoveryFields()
    {
        recoveryCode?.Clear(); recoveryPassword?.Clear(); recoveryConfirm?.Clear();
        if (txtReplacementCode != null) { txtReplacementCode.Clear(); txtReplacementCode.Visibility = Visibility.Collapsed; }
    }
    private void ShowTools(string page)
    {
        if (_toolsOperation || _isBusy || _dialogTcs != null) return;
        if (_toolsPage == "recovery" && page != "recovery") ClearRecoveryFields();
        _toolsPage = page; guideOverlay.Visibility = Visibility.Collapsed;
        toolsOverlay.Visibility = Visibility.Visible;
        toolsHomePanel.Visibility = page == "home" ? Visibility.Visible : Visibility.Collapsed;
        recoveryPanel.Visibility = page == "recovery" ? Visibility.Visible : Visibility.Collapsed;
        savesPanel.Visibility = page == "saves" ? Visibility.Visible : Visibility.Collapsed;
        playersPanel.Visibility = page == "players" ? Visibility.Visible : Visibility.Collapsed;
        advancedDiagnosticsPanel.Visibility = page == "advanced" ? Visibility.Visible : Visibility.Collapsed;
        btnToolsClose.Focus();
    }
    private void SettingsButton_Click(object sender, RoutedEventArgs e) => ShowTools("home");
    private void AdvancedDiagnosticsButton_Click(object sender, RoutedEventArgs e) => ShowTools("advanced");
    private void AdvancedCaptureOnce_Click(object sender, RoutedEventArgs e)
    {
        if (_toolsOperation || toolsOverlay.Visibility != Visibility.Visible) return;
        ToolsClose_Click(sender, e);
        if (toolsOverlay.Visibility != Visibility.Visible) CaptureOnce_Click(sender, e);
    }
    private void AdvancedExportCapture_Click(object sender, RoutedEventArgs e)
    {
        if (_toolsOperation || toolsOverlay.Visibility != Visibility.Visible) return;
        ToolsClose_Click(sender, e);
        if (toolsOverlay.Visibility != Visibility.Visible) ExportCapture_Click(sender, e);
    }
    private void ToolsClose_Click(object sender, RoutedEventArgs e)
    {
        if (_toolsOperation) return;
        ClearRecoveryFields(); toolsOverlay.Visibility = Visibility.Collapsed; btnSettings.Focus();
    }
    private void ToolsOverlay_PreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key == Key.Escape) { ToolsClose_Click(sender, e); e.Handled = true; }
    }
    private async void ForgotPasswordButton_Click(object sender, RoutedEventArgs e)
    {
        if (_toolsOperation || _isBusy || _dialogTcs != null) return;
        try
        {
            _recoveryServer = txtServer.Text.Trim(); _recoveryUser = txtUsername.Text.Trim(); _recoveryBind = _settings.BindOverride;
            _ = CredentialStore.Scope(_recoveryServer, _recoveryUser);
            ClearRecoveryFields(); txtRecoveryScope.Text = _recoveryServer + "  /  " + _recoveryUser;
            ShowTools("recovery"); recoveryCode.Focus();
        }
        catch (ArgumentException) { await ShowInfoDialogAsync(L("忘记密码", "Forgot password"), L("请先在登录页填写正确的房主地址和账号。", "Enter a valid host address and username on the sign-in page first.")); }
    }
    private void SetToolsOperation(bool value)
    {
        _toolsOperation = value; SetBusy(value);
        btnToolsClose.IsEnabled = btnRecoveryPage.IsEnabled = btnSavesPage.IsEnabled = btnPlayersPage.IsEnabled = btnAdvancedDiagnosticsPage.IsEnabled = !value;
        btnResetPassword.IsEnabled = btnRestoreOfficial.IsEnabled = btnRestoreMod.IsEnabled = !value;
        btnCaptureOnce.IsEnabled = btnExportCapture.IsEnabled = !value;
        recoveryCode.IsEnabled = recoveryPassword.IsEnabled = recoveryConfirm.IsEnabled = !value;
    }
    private async void ResetPassword_Click(object sender, RoutedEventArgs e)
    {
        if (_toolsOperation || App.PreviewMode) return;
        string server = _recoveryServer, user = _recoveryUser, bind = _recoveryBind;
        string password = recoveryPassword.Password, code = recoveryCode.Password, confirmation = recoveryConfirm.Password;
        SetToolsOperation(true);
        try
        {
            var result = await _passwordRecovery.ResetAsync(server, bind, user, code, password, confirmation, _toolsCts.Token);
            if (!result.Success)
            {
                string message = result.Code switch
                {
                    "invalid-code" => L("账号或恢复码不正确。", "Incorrect username or recovery code."),
                    "unsupported-server" => L("房主服务端尚不支持恢复密码。", "The host does not support password recovery."),
                    _ => L("未能确认重置结果。若请求已发出，密码可能已改变，请先尝试新密码；不会自动重试。", "Reset could not be confirmed. If the request was sent, the password may have changed: try the new password before retrying.")
                };
                await ShowInfoDialogAsync(L("密码恢复", "Password recovery"), message); return;
            }
            // Display the replacement even if local DPAPI persistence fails after server commit.
            ClearRecoveryFields(); txtReplacementCode.Text = result.RecoveryCode; txtReplacementCode.Visibility = Visibility.Visible;
            bool stored = true;
            try
            {
                _credentialStore.Remove(server, user);
                if (chkRememberPassword.IsChecked == true) _credentialStore.Save(server, user, password);
            }
            catch { stored = false; }
            if (CanUseCapturedPassword(CredentialStore.Scope(server, user), txtServer.Text, txtUsername.Text))
            {
                _restoringPassword = true;
                try { txtPassword.Password = password; _autoPasswordScope = CredentialStore.Scope(server, user); }
                finally { _restoringPassword = false; }
            }
            await ShowInfoDialogAsync(L("密码已重置", "Password reset"), L("新恢复码已显示在设置页，请立即复制保存。关闭页面后不会保留。", "Save the new recovery code shown in Settings. It is cleared when the page closes.") + (stored ? "" : L("\n本机密码保存失败，请下次手动输入新密码。", "\nLocal password storage failed; enter the new password manually next time.")));
        }
        catch (ArgumentException) { await ShowInfoDialogAsync(L("检查输入", "Check input"), L("请填写有效账号、恢复码及至少一个字符的密码，两次密码必须一致。", "Enter a valid username, recovery code, and matching passwords of at least one character.")); }
        catch { await ShowInfoDialogAsync(L("密码恢复", "Password recovery"), L("操作未能确认完成。请检查连接并尝试新密码，不会自动重试。", "Completion could not be confirmed. Check the connection and try the new password; no automatic retry was made.")); }
        finally { SetToolsOperation(false); }
    }
    private void RestoreSavesButton_Click(object sender, RoutedEventArgs e) => ShowTools("saves");
    private async void RestoreOfficial_Click(object sender, RoutedEventArgs e) => await RestoreSavesAsync("Official");
    private async void RestoreMod_Click(object sender, RoutedEventArgs e) => await RestoreSavesAsync("MOD");
    private async Task RestoreSavesAsync(string mode)
    {
        if (_toolsOperation || App.PreviewMode) return;
        if (GameRuntime.AnyGameRunning()) { await ShowInfoDialogAsync(L("请先退出游戏", "Exit game first"), L("不会强制结束游戏。请保存并退出，再恢复存档。", "Save and exit the game before restoring. The launcher will not force it to stop.")); return; }
        var answer = await ShowConfirmDialogAsync(L("确认覆盖存档", "Confirm save replacement"), mode + "\n" + _saveGameService.DirectoryFor(mode) + "\n\n" + L("将替换此模式的两个存档槽。当前文件会先备份到启动器 data 下；另一模式和其他文件不变。继续？", "Both slots of this mode will be replaced. Current files are backed up under launcher data; the other mode and unrelated files remain unchanged. Continue?"), L("确认覆盖存档", "Restore saves"));
        if (answer != MessageBoxResult.Yes) return;
        SetToolsOperation(true);
        try
        {
            string backup = await Task.Run(() => _saveGameService.RestoreBundled(mode, RuntimePaths.Root));
            await ShowInfoDialogAsync(L("存档已恢复", "Saves restored"), mode + L(" 存档已写入并核对。\n原文件备份：", " saves written and verified.\nPrevious files: ") + backup);
        }
        catch (Exception ex) { await ShowInfoDialogAsync(L("存档恢复未完成", "Save restore incomplete"), ex is IOException or ArgumentException ? ex.Message : L("文件操作失败，未确认完成；请检查备份目录。", "File operation failed; completion is unconfirmed. Check the backup directory.")); }
        finally { SetToolsOperation(false); }
    }
    private async void PlayersButton_Click(object sender, RoutedEventArgs e) { ShowTools("players"); await RefreshPresenceAsync(); }
    private async void RefreshPlayers_Click(object sender, RoutedEventArgs e) => await RefreshPresenceAsync();
    private Task RefreshPresenceAsync()
    {
        if (_presencePending) return _presenceTask;
        return _presenceTask = RefreshPresenceCoreAsync();
    }
    private async Task RefreshPresenceCoreAsync()
    {
        if (App.PreviewMode || _presencePending || _toolsCts.IsCancellationRequested) return;
        _presenceMode = GetSelectedGameplayMode(); _presenceInGame = _isGameRunning;
        string server = txtServer.Text.Trim(), bind = _settings.BindOverride;
        if (string.IsNullOrWhiteSpace(server)) { _lastPresence = null; RenderPresence(); return; }
        _presencePending = true;
        try
        {
            var snapshot = await _presenceClient.ProbeAsync(server, bind, txtUsername.Text, _presenceMode, _presenceInGame, _toolsCts.Token);
            if (_toolsCts.IsCancellationRequested || server != txtServer.Text.Trim() || bind != _settings.BindOverride) return;
            _lastPresence = snapshot; RenderPresence();
        }
        catch (OperationCanceledException) { }
        catch { if (!_toolsCts.IsCancellationRequested) { _lastPresence = null; RenderPresence(); } } // No stale UI writes during shutdown.
        finally { _presencePending = false; }
    }
    private void RenderPresence()
    {
        if (btnPlayersSummary == null) return;
        var snapshot = _lastPresence;
        btnPlayersSummary.Content = snapshot?.Replied == true ? L($"在线 {snapshot.Players.Length} · RTT {snapshot.Quality.RttMs:0} ms", $"Online {snapshot.Players.Length} · RTT {snapshot.Quality.RttMs:0} ms") : L("在线玩家 / 联机质量", "Players / quality");
        if (snapshot == null) { txtQualityDetails.Text = L("填写房主地址后开始检测。", "Enter a host address to start probing."); playersList.ItemsSource = null; return; }
        var q = snapshot.Quality;
        string state = q.State switch { "good" => L("良好", "Good"), "fair" => L("一般", "Fair"), "unstable" => L("不稳定", "Unstable"), "no-reply" => L("无响应", "No reply"), _ => L("采样中", "Sampling") };
        string n(double? value) => value?.ToString("0.0") ?? "—";
        txtQualityDetails.Text = (snapshot.Replied ? state : L("本轮无响应：可能是旧版房主或探针受阻；下列玩家信息可能已过期。", "No reply this round: old host or blocked probe; cached player rows may be stale.")) + $"\nRTT {n(q.RttMs)} ms · " + L("抖动", "Jitter") + $" {n(q.JitterMs)} ms · " + L("丢包", "Loss") + $" {q.LossPercent:0}% · N={q.Samples}";
        playersList.ItemsSource = snapshot.Players.Select(p => $"{p.Name}  ·  {p.Mode}  ·  " + (p.InGame ? L("游戏中", "In game") : L("启动器在线", "Launcher online")) + $"  ·  {p.Address}  ·  {n(p.RttMs)} ms").ToArray();
    }
    private void ReplyToPresence(byte[] buffer, IPEndPoint remote)
    {
        if (!_isHostModeCached || !IsDedicatedServerRunning() || _hostSessionId is not { } session || _currentHostBroadcastIp is not { } ip) return;
        byte[]? response = _presenceRegistry.Handle(buffer, remote, session, _currentHostBroadcastName ?? "Host", ip, _hostRuntime.ApiPort, _presenceMode, _presenceInGame);
        if (response == null) return;
        try { using var sender = CreateDiscoveryUdpClient(ip, 0); sender.Send(response, response.Length, remote); }
        catch (SocketException) { } // A failed quality probe must not stop host discovery.
    }
    internal void ShowToolsPreview(string page)
    {
        txtServer.Text = "192.0.2.10:50051"; txtUsername.Text = "PreviewPlayer";
        ApplyToolsLocalization(); ShowTools(page);
        if (page == "recovery") txtRecoveryScope.Text = "192.0.2.10:50051 / PreviewPlayer";
        if (page == "players")
        {
            _lastPresence = new(true, [new("a", "PreviewHost", "192.0.2.10", "MOD", true, 0), new("b", "PreviewPlayer", "192.0.2.11", "MOD", true, 8.2)], new(10, 10, 8.2, 1.1, 0, "good")); RenderPresence();
        }
    }
}

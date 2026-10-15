using Microsoft.Win32;
using SplinterCellCNLauncher.Models;
using SplinterCellCNLauncher.Services;
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Collections.Generic;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Net.NetworkInformation;
using System.Reflection;
using System.Text.RegularExpressions;
using System.Threading;
using System.Threading.Tasks;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;

namespace SplinterCellCNLauncher;

public partial class MainWindow : Window
{
    private readonly LauncherSettingsService _settingsService = new();
    private readonly NetworkAdapterService _networkService = new();
    private readonly AuthService _authService = new();
    private readonly GameLocatorService _gameLocatorService = new();


    private readonly SaveGameService _saveGameService = new();
    private readonly GameLaunchService _gameLaunchService = new();
    private readonly FirewallService _firewallService = new();
    private readonly NetworkRiskService _networkRiskService = new();
    private readonly MediaPlayer _musicPlayer = new();

    private LauncherSettings _settings = new();
    private string _gameDir = "";
    private bool _statusRefreshRunning;
    private bool _isGameRunning;
    private string? _runningGameProcessName;
    private Guid? _unconfirmedLaunchSession;
    private bool _isEndingGame;
    private bool _allowClose;
    private bool _closingBackgroundTasks;
    // One-shot UI work stays on the dispatcher context and remains owned until drained.
    private readonly CancellationTokenSource _windowOneShotCts = new();
    private Task _statusRefreshTask = Task.CompletedTask;
    private Task _windowLoadedTask = Task.CompletedTask;
    private Task _networkCheckTask = Task.CompletedTask;
    private bool _windowLoadedStarted;
    private bool _networkCheckRunning;
    private bool _isGameStarting;
    private DateTime _launchStartedUtc=DateTime.MaxValue;
    private bool _isBusy;
    private readonly OwnedBackgroundTask _gameMonitor = new(ex => LogService.Error("gameMonitor task failed: " + ex));
    private readonly OwnedBackgroundTask _lanMonitor = new(ex => LogService.Error("lanMonitor task failed: " + ex));
    private bool _lastServerReachable;
    private bool _lastLanLocalReady;
    private DateTime _lastAutoServerCheckUtc = DateTime.MinValue;
    private ServerStatusKind _serverStatusKind = ServerStatusKind.Unknown;
    private TaskCompletionSource<MessageBoxResult>? _dialogTcs;
    private IInputElement? _dialogPreviousFocus;
    private readonly HostRuntimeService _hostRuntime=new();
    private string _hostBindIp="unselected";
    private readonly OwnedBackgroundTask _hostBroadcast = new(ex => LogService.Error("hostBroadcast task failed: " + ex));
    private readonly OwnedBackgroundTask _hostDiscovery = new(ex => LogService.Error("hostDiscovery task failed: " + ex));
    private string? _hostSessionId;
    private string? _currentHostBroadcastName;
    private string? _currentHostBroadcastIp;
    private sealed record HostBroadcastIntent(string Name, string Ip, string BindOverride, int ApiPort);
    private HostBroadcastIntent? _hostBroadcastIntent;
    private bool _isChangingConnectionMode;
    // 后台广播/自动发现线程不能直接读取 WPF 控件，
    // 用这个缓存记录当前是否为房主模式，避免跨线程访问 UI。
    private volatile bool _isHostModeCached;
    private bool _suppressUsernameTextChanged;
    private bool _suppressServerTextChanged;
    private bool _serverAddressEditedAfterLastCheck;
    private string _lastNonHostServerAddress = "";
    private string? _lastHostSwitchPromptKey;
    private string? _lastSingleHostAutoFillPromptKey;
    private bool _multiHostDialogShownThisSession;
    private bool _isHostSelectionDialogOpen;
    private string _networkTarget="";
    private int _guideIndex;
    private List<GuideStep> _guideSteps = new();
    private bool _launcherMediaFrozenForGameSession;
    private bool _musicIsPlaying;
    private bool _musicProbeMuted;
    private bool _musicPlayedThisSession;
    private readonly Dictionary<string, DiscoveredHost> _discoveredHosts = new(StringComparer.OrdinalIgnoreCase);

    private const int GameStableAppearSeconds = 20;
    private const int GameExitMissingChecks = 2;
    private const int GameProcessProbeIntervalMs = 1000;
    private const int HostBroadcastPort = 47777;
    private const int HostBroadcastIntervalMs = 2000;
    private const int HostDiscoveryExpireSeconds = 10;
    private const int HostHeartbeatTimeoutSeconds = 10;
    private const string ServerPlaceholderNormal = "可手动输入 IP 地址 / 域名，或等待自动发现";


    private enum ServerStatusKind
    {
        Unknown,
        Checking,
        Normal,
        LocalAbnormal,
        ServerFailed
    }





    private sealed class LanLocalStatus
    {
        public BindIpCandidate? Bind {get;init;}
        public string Error {get;init;} = "";
        public bool IsOk => Bind != null;
    }

    private sealed class DiscoveredHost
    {
        public string SessionId { get; init; } = "";
        public string HostName { get; init; } = "";
        public string HostIp { get; init; } = "";
        public int ApiPort { get; init; } = 50051;
        public string Endpoint => $"{HostIp}:{ApiPort}";
        public DateTime LastSeenUtc { get; set; } = DateTime.UtcNow;
        public string DisplayName => string.IsNullOrWhiteSpace(HostName) ? Endpoint : $"{HostName}（{Endpoint}）";
    }

    private sealed class GuideStep
    {
        public FrameworkElement Target { get; init; } = null!;
        public string TitleZh { get; init; } = "";
        public string TitleEn { get; init; } = "";
        public string MessageZh { get; init; } = "";
        public string MessageEn { get; init; } = "";
    }

    public MainWindow()
    {
        InitializeComponent();
        if (App.PreviewMode) { cmbBindAdapter.Items.Add(new ComboBoxItem {Content="自动按服务器路由",Tag=""});cmbBindAdapter.SelectedIndex=0;ApplyLocalization();return; }

        ForceEnglishInputForPlainTextBoxes();
        LoadSettingsToUi();
        _networkTarget=txtServer.Text.Trim();
        PopulateAdapters();
        DetectNetworkSilently();
        StartLanMonitor();
        StartHostDiscoveryListener();
        StartLanTools();
        UpdateConnectionModeUi();
        ApplyLocalization();
        SetGameRunningState(false);
        PlayStartupMusicIfEnabled();
    }




    private bool IsEnglish => string.Equals(_settings.Language,"en-US", StringComparison.OrdinalIgnoreCase);

    private string L(string zh, string en) => IsEnglish ? en : zh;

    private string LocalizeStatusText(string text)
    {
        return text switch
        {
            "未检测" => L("未检测", "Not checked"),
            "检测中..." => L("检测中...", "Checking..."),
            "服务器连接正常" => L("服务器连接正常", "Server connected"),
            "服务器连接异常" => L("服务器连接异常", "Server abnormal"),
            "服务器连接失败" => L("服务器连接失败", "Server failed"),
            _ => text
        };
    }

    private string LocalizeBusyText(string text)
    {
        return text switch
        {
            "启动服务端..." => L("启动服务端...", "Starting server..."),
            "准备启动..." => L("准备启动...", "Preparing..."),
            "清理冲突..." => L("清理冲突...", "Cleaning conflicts..."),
            "检测网络..." => L("检测网络...", "Checking network..."),
            "校验游戏..." => L("校验游戏...", "Checking game..."),
            "连接服务器..." => L("连接服务器...", "Connecting server..."),
            "注册账号..." => L("注册账号...", "Registering account..."),
            "部署组件..." => L("检查并补齐组件...", "Checking / repairing components..."),
            "检查存档..." => L("检查存档...", "Checking saves..."),
            "写入配置..." => L("写入配置...", "Writing config..."),
            "启动游戏..." => L("启动游戏...", "Starting game..."),
            _ => text
        };
    }

    private string LocalizeDialogButtonText(string text)
    {
        return text switch
        {
            "确定" => L("确定", "OK"),
            "知道了" => L("知道了", "OK"),
            "取消" => L("取消", "Cancel"),
            "返回" => L("返回", "Back"),
            "确认切换" => L("确认切换", "Switch"),
            "继续切换" => L("继续切换", "Switch"),
            "关闭并退出" => L("关闭并退出", "Close and Exit"),
            "取消等待" => L("取消等待", "Cancel Waiting"),
            "结束游戏" => L("结束游戏", "End Game"),
            "重新启动" => L("重新启动", "Restart"),
            "我知道了，继续启动" => L("我知道了，继续启动", "Continue Anyway"),
            "取消启动" => L("取消启动", "Cancel Launch"),
            "注册账号" => L("注册账号", "Register"),
            "继续" => L("继续", "Continue"),
            "自动填写" => L("自动填写", "Auto Fill"),
            "切换" => L("切换", "Switch"),
            "忽略" => L("忽略", "Ignore"),
            "连接房主" => L("连接房主", "Connect Host"),
            "我已了解，继续覆盖" => L("我已了解，继续覆盖", "I Understand, Continue"),
            "确认覆盖存档" => L("确认覆盖存档", "Confirm Overwrite"),
            _ => text
        };
    }

    private string LocalizeDialogTitleText(string title)
    {
        return title switch
        {
            "请先填写账号名" => L("请先填写账号名", "Username Required"),
            "切换为房主模式" => L("切换为房主模式", "Switch to Host Mode"),
            "切换联机模式" => L("切换联机模式", "Switch Connection Mode"),
            "房主模式启动失败" => L("房主模式启动失败", "Host Mode Failed"),
            "服务器连接异常" => L("服务器连接异常", "Server Abnormal"),
            "服务器连接失败" => L("服务器连接失败", "Server Failed"),
            "未填写服务器地址" => L("未填写服务器地址", "Server Address Required"),
            "服务器地址格式不正确" => L("服务器地址格式不正确", "Invalid Server Address"),
            "检测到可能影响联机的网络环境" => L("检测到可能影响联机的网络环境", "Network Risk Detected"),
            "账号不存在" => L("账号不存在", "Account Not Found"),
            "启动失败" => L("启动失败", "Launch Failed"),
            "联机组件未确认生效" => L("联机组件未确认生效", "Online Hook Not Confirmed"),
            "发现房主" => L("发现房主", "Host Found"),
            "发现新房主" => L("发现新房主", "New Host Found"),
            "发现多个房主" => L("发现多个房主", "Multiple Hosts Found"),
            "游戏运行中" => L("游戏运行中", "Game Is Running"),
            "暂无可选房主" => L("暂无可选房主", "No Hosts Available"),
            "游戏正在启动" => L("游戏正在启动", "Game Is Starting"),
            "游戏仍在运行" => L("游戏仍在运行", "Game Is Running"),
            "房主服务端仍在运行" => L("房主服务端仍在运行", "Host Server Still Running"),
            "重新启动游戏" => L("重新启动游戏", "Restart Game"),
            "结束游戏" => L("结束游戏", "End Game"),
            "选择房主" => L("选择房主", "Select Host"),
            "当前发现的房主" => L("当前发现的房主", "Available Host"),
            "检测到已有本地存档" => L("检测到已有本地存档", "Existing Saves Detected"),
            "再次确认覆盖存档" => L("再次确认覆盖存档", "Confirm Save Overwrite Again"),
            "存档已覆盖" => L("存档已覆盖", "Saves Overwritten"),
            "存档检查失败" => L("存档检查失败", "Save Check Failed"),
            _ => title
        };
    }

    private void ApplyLocalization()
    {
        try
        {
            txtTopBadge.Text = "1.0 · DX11 · LAN ONLY";
            txtAppTitle.Text = "SCBL LAN";
            txtSectionTitle.Text = L("SCBL 登录", "SCBL SIGN IN");
            txtConnectionModeLabel.Text = L("联机模式", "Connection Mode");
            itemClientMode.Content = L("加入房主", "Join host");
            itemHostMode.Content = L("创建房主", "Create host");
            txtUsernameLabel.Text = L("账号", "Username");
            txtPasswordLabel.Text = L("密码", "Password");
            txtBindAdapterLabel.Text = L("联机网卡 / IPv4", "Network Adapter / IPv4");
            foreach (var automatic in cmbBindAdapter.Items.OfType<ComboBoxItem>().Where(x => x.Tag as string == ""))
                automatic.Content = L("自动按服务器路由（开房多网卡时须选择）", "Automatic route (choose an adapter for hosting)");
            btnGuide.ToolTip = L("使用指引", "Quick guide");
            btnWindowMinimize.ToolTip = L("最小化", "Minimize");
            btnWindowClose.ToolTip = L("关闭", "Close");
            cmbGameplayMode.ToolTip = L("官方：不加载玩法插件；MOD：需要完整的本版 gameplay 模块。队友请使用相同游戏模式。", "Official: no gameplay plugin. MOD: requires this release's gameplay module. Teammates should use the same mode.");
            chkRememberPassword.Content = L("记住密码", "Remember password");
            chkRememberPassword.ToolTip = L("仅当前 Windows 用户可解密；取消将清除本机启动器保存的密码。", "Encrypted for this Windows user; unchecking clears this launcher’s saved passwords.");
            txtServerLabel.Text = L("服务器地址", "Server Address");
            txtHostSwitchSuffix.Text = L("  切换 ▾", "  Switch ▾");
            txtConnectionStatusCaption.Text = L("连接状态", "Connection Status");
            btnCheckNetwork.Content = L("检测网络", "Check Network");
            btnExportDiagnostics.Content = L("导出诊断包", "Export diagnostics");
            btnExportDiagnostics.ToolTip = L("包含现存日志全文及字段脱敏持久配置；仍可能含敏感信息。历史有效配置快照尚未实施，请查看清单后分享。不会自动上传。", "Includes retained full log text and typed redacted persisted configs. Sensitive data may remain. Historical effective snapshots not implemented; review the manifest before sharing. No upload.");
            txtGameplayModeLabel.Text = L("游戏模式", "Game Mode");
            txtFovLabel.Text = L("视野范围（FOV）", "Field of View (FOV)");
            btnFovReset.Content = L("默认", "Reset");
            itemGameplayOfficial.Content = L("官方", "Official");
            btnLanguageToggle.Content = "中文 / EN";
            btnGuide.Content = "?";
            ApplyToolsLocalization();
            UpdateMusicButton();
            UpdateServerAddressInputState(_lastLanLocalReady);
            txtServerStatus.Text = LocalizeStatusText(txtServerStatus.Text);
            SetBusy(_isBusy);
        }
        catch (Exception ex)
        {
            LogService.Error($"ApplyLocalization failed: {ex}");
        }
    }

    private void LanguageToggleButton_Click(object sender, RoutedEventArgs e)
    {
        string previous = _settings.Language;
        string next = IsEnglish ? "zh-CN" : "en-US";
        if (!TryPersistInteractiveSetting(() => _settings.Language = next,
            () => _settings.Language = previous)) return;
        ApplyLocalization();
        if (guideOverlay.Visibility == Visibility.Visible)
            RefreshGuideStep();
        LogService.Info($"Language switched to {_settings.Language}");
    }

    private void MusicToggleButton_Click(object sender, RoutedEventArgs e)
    {
        bool previous = _settings.MusicEnabled;
        if (!TryPersistInteractiveSetting(() => _settings.MusicEnabled = !previous,
            () => _settings.MusicEnabled = previous)) return;
        UpdateMusicButton();
        if (!_settings.MusicEnabled) StopMusic();
        else PlayStartupMusicIfEnabled(forceReplay: true);
    }

    private void UpdateMusicButton()
    {
        if (btnMusicToggle == null) return;
        btnMusicToggle.Content = _settings.MusicEnabled ? "\uE767" : "\uE74F";
        string state = _settings.MusicEnabled ? L("背景音乐：开", "Background music: On") : L("背景音乐：关", "Background music: Off");
        btnMusicToggle.ToolTip = state + "\n" + L("播放一次；启动游戏后停播，本次启动器会话不再恢复。", "Plays once. Starting a game silences music until the launcher is restarted.");
    }

    private void PauseMusicForGameSession()
    {
        if (_launcherMediaFrozenForGameSession) return;
        _launcherMediaFrozenForGameSession = true;
        // Same session policy as SCBL 3.0; close rather than hold a paused decoder.
        StopMusic();
        LogService.Info("Launcher BGM stopped for the game session; no resume until launcher restart.");
    }

    private void PlayStartupMusicIfEnabled(bool forceReplay = false)
    {
        if (!_settings.MusicEnabled || _launcherMediaFrozenForGameSession || _closingBackgroundTasks) return;
        if (_musicPlayedThisSession && !forceReplay) return;
        try
        {
            string? musicPath = ResolveLauncherMusicPath();
            if (string.IsNullOrWhiteSpace(musicPath) || !File.Exists(musicPath)) return;
            if (forceReplay) StopMusic();
            _musicPlayer.MediaEnded -= MusicPlayer_MediaEnded;
            _musicPlayer.MediaEnded += MusicPlayer_MediaEnded;
            _musicPlayer.MediaFailed -= MusicPlayer_MediaFailed;
            _musicPlayer.MediaFailed += MusicPlayer_MediaFailed;
            _musicPlayer.Volume = _musicProbeMuted ? 0 : 0.30;
            _musicPlayer.Open(new Uri(musicPath, UriKind.Absolute));
            _musicPlayer.Play();
            _musicPlayedThisSession = true;
            _musicIsPlaying = true;
        }
        catch (Exception ex)
        {
            StopMusic();
            LogService.Error("Failed to play launcher music: " + ex.GetType().Name);
        }
    }

    private void MusicPlayer_MediaEnded(object? sender, EventArgs e) => StopMusic();

    private void MusicPlayer_MediaFailed(object? sender, ExceptionEventArgs e)
    {
        StopMusic();
        LogService.Error("Launcher music unavailable: " + e.ErrorException.GetType().Name);
    }

    private string? ResolveLauncherMusicPath()
    {
        // Keep the release root clean. Only this exact embedded asset is used.
        // A content-addressed per-user cache avoids rewriting a file being decoded.
        string? temporary = null;
        try
        {
            using var input = EmbeddedResourceService.OpenEmbeddedFileStrict("launcher_bgm.mp3");
            using var buffer = new MemoryStream(); input.CopyTo(buffer);
            byte[] bytes = buffer.ToArray();
            string hash = Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(bytes));
            string directory = Path.Combine(LogService.AppDataDir, "media");
            string path = Path.Combine(directory, "launcher_bgm-" + hash + ".mp3");
            if (File.Exists(path) && Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(File.ReadAllBytes(path))) == hash) return path;
            Directory.CreateDirectory(directory);
            temporary = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
            using (var output = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None)) output.Write(bytes);
            File.Move(temporary, path, overwrite: true);
            return path;
        }
        catch (Exception ex)
        {
            LogService.Error("Launcher music asset unavailable: " + ex.GetType().Name);
            return null;
        }
        finally
        {
            if (temporary != null) { try { File.Delete(temporary); } catch { } }
        }
    }

    private void StopMusic()
    {
        _musicIsPlaying = false;
        _musicPlayer.MediaEnded -= MusicPlayer_MediaEnded;
        _musicPlayer.MediaFailed -= MusicPlayer_MediaFailed;
        try { _musicPlayer.Volume = 0; _musicPlayer.Stop(); }
        catch (Exception ex) { LogService.Error("Launcher music stop failed: " + ex.GetType().Name); }
        finally { try { _musicPlayer.Close(); } catch { } }
    }

    private void GuideButton_Click(object sender, RoutedEventArgs e)
    {
        ShowGuide(markCompletedOnClose: false);
    }

    private void ShowGuide(bool markCompletedOnClose)
    {
        try
        {
            BuildGuideSteps();
            if (_guideSteps.Count == 0)
                return;

            guideOverlay.Tag = markCompletedOnClose;
            _guideIndex = 0;
            guideOverlay.Visibility = Visibility.Visible;
            RefreshGuideStep();
        }
        catch (Exception ex)
        {
            LogService.Error($"ShowGuide failed: {ex}");
        }
    }

    private void BuildGuideSteps()
    {
        _guideSteps = new List<GuideStep>
        {
            new() {Target=cmbConnectionMode,TitleZh="开房或加入",TitleEn="Host or join",MessageZh="房主模式启动本机独立服务端；非房主模式连接队友地址。切换退出房主模式会停止本启动器的服务端。",MessageEn="Host locally or join a peer. Leaving host mode stops only this launcher's server."},
            new() {Target=txtUsername,TitleZh="本服务器账号",TitleEn="Local server account",MessageZh="可在当前服务器注册账号。勾选记住密码后，成功登录的密码由当前 Windows 用户加密保存；不同服务器和账号独立。",MessageEn="Register on this server. Remember password encrypts successful logins for this Windows user, separately for each server and account."},
            new() {Target=txtServer,TitleZh="服务器地址",TitleEn="Server address",MessageZh="可以手动输入 IPv4 / 域名。自动发现只是便利功能，组网工具不转发广播时仍可手动连接。",MessageEn="Enter IPv4 or a hostname. Discovery is optional on routed networks."},
            new() {Target=cmbBindAdapter,TitleZh="选择本机网卡",TitleEn="Choose local interface",MessageZh="加入时默认按服务器路由选择；多网卡开房时请选择队友能够访问的本机地址，不按组网软件名称判断。",MessageEn="Joining follows the server route. On a multi-interface host choose a reachable local address."},
            new() {Target=btnCheckNetwork,TitleZh="就绪与联机不同",TitleEn="Readiness is not gameplay",MessageZh="检测验证兼容服务端和必要服务已就绪；双向游戏 UDP、多人任务及迁主还需实机确认。",MessageEn="Service readiness does not prove bidirectional game UDP or host migration."},
            new() {Target=btnLaunch,TitleZh="启动与还原",TitleEn="Launch and restore",MessageZh="校验 Hook 后启动游戏；未知组件不会直接覆盖。存档保持原位置，不会部署或覆盖基础存档。",MessageEn="Launch with verified Hooks. Unknown components are not overwritten and saves remain game-owned."}
        };
    }

    private void RefreshGuideStep()
    {
        if (_guideSteps.Count == 0 || guideOverlay.Visibility != Visibility.Visible)
            return;

        _guideIndex = Math.Max(0, Math.Min(_guideIndex, _guideSteps.Count - 1));
        GuideStep step = _guideSteps[_guideIndex];

        txtGuideStep.Text = $"{_guideIndex + 1} / {_guideSteps.Count}";
        txtGuideTitle.Text = IsEnglish ? step.TitleEn : step.TitleZh;
        txtGuideMessage.Text = IsEnglish ? step.MessageEn : step.MessageZh;
        btnGuideSkip.Content = L("跳过", "Skip");
        btnGuidePrev.Content = L("上一步", "Back");
        btnGuideNext.Content = _guideIndex >= _guideSteps.Count - 1 ? L("完成", "Done") : L("下一步", "Next");
        btnGuidePrev.IsEnabled = _guideIndex > 0;

        PositionGuideVisuals(step.Target);
    }

    private void PositionGuideVisuals(FrameworkElement target)
    {
        try
        {
            target.UpdateLayout();
            rootGrid.UpdateLayout();

            double windowWidth = rootGrid.ActualWidth > 0 ? rootGrid.ActualWidth : ActualWidth;
            double windowHeight = rootGrid.ActualHeight > 0 ? rootGrid.ActualHeight : ActualHeight;
            if (windowWidth <= 0) windowWidth = Width;
            if (windowHeight <= 0) windowHeight = Height;

            Point topLeft = target.TranslatePoint(new Point(0, 0), rootGrid);
            double targetWidth = Math.Max(40, target.ActualWidth);
            double targetHeight = Math.Max(28, target.ActualHeight);
            double pad = 8;

            double highlightLeft = Clamp(topLeft.X - pad, 10, Math.Max(10, windowWidth - 20));
            double highlightTop = Clamp(topLeft.Y - pad, 10, Math.Max(10, windowHeight - 20));
            double highlightWidth = Math.Min(targetWidth + pad * 2, Math.Max(40, windowWidth - highlightLeft - 10));
            double highlightHeight = Math.Min(targetHeight + pad * 2, Math.Max(28, windowHeight - highlightTop - 10));

            Canvas.SetLeft(guideHighlight, highlightLeft);
            Canvas.SetTop(guideHighlight, highlightTop);
            guideHighlight.Width = highlightWidth;
            guideHighlight.Height = highlightHeight;

            // 指引卡片不使用滚动条，文案保持精简；卡片尽量贴近正在高亮的控件。
            double cardWidth = Math.Min(guideCard.Width > 0 ? guideCard.Width : 330, Math.Max(260, windowWidth - 24));
            guideCard.Width = cardWidth;
            guideCard.Measure(new Size(cardWidth, double.PositiveInfinity));
            double cardHeight = guideCard.DesiredSize.Height > 0 ? guideCard.DesiredSize.Height : 220;
            cardHeight = Math.Min(cardHeight, Math.Max(190, windowHeight - 32));

            double margin = 14;

            // 水平方向：优先让卡片中心对齐当前高亮框中心，避免卡片离指引区域太远。
            double preferredLeft = highlightLeft + highlightWidth / 2 - cardWidth / 2;
            double left = Clamp(preferredLeft, margin, Math.Max(margin, windowWidth - cardWidth - margin));

            double belowTop = highlightTop + highlightHeight + 10;
            double aboveTop = highlightTop - cardHeight - 10;

            double top;
            bool cardBelowTarget;
            if (belowTop + cardHeight <= windowHeight - margin)
            {
                top = belowTop;
                cardBelowTarget = true;
            }
            else if (aboveTop >= margin)
            {
                top = aboveTop;
                cardBelowTarget = false;
            }
            else
            {
                // 上下空间都不完整时，也尽量贴近目标，不做滚动，整体限制在启动器窗口内部。
                double preferredTop = highlightTop + highlightHeight / 2 - cardHeight / 2;
                top = Clamp(preferredTop, margin, Math.Max(margin, windowHeight - cardHeight - margin));
                cardBelowTarget = top >= highlightTop;
            }

            top = Clamp(top, margin, Math.Max(margin, windowHeight - cardHeight - margin));

            Canvas.SetLeft(guideCard, left);
            Canvas.SetTop(guideCard, top);

            // 用小箭头明确告诉玩家当前高亮框才是被讲解的位置。
            guideArrow.Text = cardBelowTarget ? "▲" : "▼";
            guideArrow.Measure(new Size(32, 32));
            double arrowLeft = Clamp(highlightLeft + highlightWidth / 2 - 8, left + 10, Math.Max(left + 10, left + cardWidth - 26));
            double arrowTop = cardBelowTarget ? top - 19 : top + cardHeight - 2;
            arrowTop = Clamp(arrowTop, margin, Math.Max(margin, windowHeight - 22));
            Canvas.SetLeft(guideArrow, arrowLeft);
            Canvas.SetTop(guideArrow, arrowTop);
        }
        catch (Exception ex)
        {
            LogService.Error($"PositionGuideVisuals failed: {ex}");
            Canvas.SetLeft(guideCard, 40);
            Canvas.SetTop(guideCard, 360);
            Canvas.SetLeft(guideArrow, 70);
            Canvas.SetTop(guideArrow, 342);
        }
    }

    private static double Clamp(double value, double min, double max)
    {
        if (max < min)
            return min;

        if (value < min)
            return min;

        return value > max ? max : value;
    }

    private void GuideNextButton_Click(object sender, RoutedEventArgs e)
    {
        if (_guideIndex >= _guideSteps.Count - 1)
        {
            CloseGuide();
            return;
        }

        _guideIndex++;
        RefreshGuideStep();
    }

    private void GuidePrevButton_Click(object sender, RoutedEventArgs e)
    {
        if (_guideIndex <= 0)
            return;

        _guideIndex--;
        RefreshGuideStep();
    }

    private void GuideSkipButton_Click(object sender, RoutedEventArgs e)
    {
        CloseGuide();
    }

    private void CloseGuide()
    {
        bool markCompleted = guideOverlay.Tag is bool b && b;
        guideOverlay.Visibility = Visibility.Collapsed;

        if (markCompleted && !_settings.GuideCompleted)
        {
            bool previous = _settings.GuideCompleted;
            TryPersistInteractiveSetting(() => _settings.GuideCompleted = true,
                () => _settings.GuideCompleted = previous);
        }
    }

    private void ForceEnglishInputForPlainTextBoxes()
    {
        // TextBox 在部分中文输入法下可能绕过 PreviewTextInput，先从输入法层面禁用 IME。
        // PasswordBox 本身表现正常，这里只处理账号框和服务器地址框。
        InputMethod.SetIsInputMethodEnabled(txtUsername, false);
        InputMethod.SetPreferredImeState(txtUsername, InputMethodState.Off);
        txtUsername.InputScope = CreateInputScope(InputScopeNameValue.AlphanumericHalfWidth);

        InputMethod.SetIsInputMethodEnabled(txtServer, false);
        InputMethod.SetPreferredImeState(txtServer, InputMethodState.Off);
        txtServer.InputScope = CreateInputScope(InputScopeNameValue.Url);
    }

    private static InputScope CreateInputScope(InputScopeNameValue value)
    {
        var scope = new InputScope();
        scope.Names.Add(new InputScopeName(value));
        return scope;
    }

    private async void Window_Loaded(object sender, RoutedEventArgs e)
    {
        if (App.PreviewMode || _closingBackgroundTasks || _windowLoadedStarted) return;
        _windowLoadedStarted = true;
        await (_windowLoadedTask = WindowLoadedAsync());
    }

    private async Task WindowLoadedAsync()
    {
        try
        {
            await Dispatcher.InvokeAsync(() =>
            {
                if (!_closingBackgroundTasks) BringLauncherToFront("launcher loaded");
            }, System.Windows.Threading.DispatcherPriority.ApplicationIdle);
            if (_closingBackgroundTasks) return;

            _firewallService.EnsureFirewallRulesBestEffort(GetLauncherBaseDirectory(), _gameDir);
            await RefreshServerStatusAsync();
            if (_closingBackgroundTasks) return;

            if (!_settings.GuideCompleted)
            {
                await Task.Delay(450, _windowOneShotCts.Token);
                if (_closingBackgroundTasks) return;
                BringLauncherToFront("show guide");
                ShowGuide(markCompletedOnClose: true);
            }
        }
        catch (OperationCanceledException) when (_windowOneShotCts.IsCancellationRequested) { }
        catch (Exception ex) { LogService.Error("Launcher loaded task failed: " + ex); }
    }

    private async void Window_Closing(object? sender, CancelEventArgs e)
    {
        if(App.PreviewMode) return;
        if (_toolsOperation || _isExportingDiagnostics) { e.Cancel = true; return; }
        if (_allowClose)
        {
            return;
        }

        if (_closingBackgroundTasks) { e.Cancel = true; return; }

        // Launch/host preparation has not yet acquired a cancellable game session.
        // Keep the window alive until that one-shot operation reaches its existing boundary.
        if (_isBusy && !_isGameStarting && !_isGameRunning) { e.Cancel = true; return; }

        if (_isGameStarting)
        {
            e.Cancel = true;

            var result = await ShowConfirmDialogAsync(
                title: "游戏正在启动",
                message: "当前游戏仍在启动中。\n\n如果 Uplay 登录较慢或网络较慢，请不要关闭启动器。\n是否取消等待并关闭启动器？",
                yesText: "取消等待",
                noText: "返回");

            if (result != MessageBoxResult.Yes)
                return;

            _gameLaunchService.SetDiagnosticIntent(RunIntent.Cancelled);
            CancelGameMonitor();
            SetGameRunningState(false);

            await CompleteCloseAsync();
            return;
        }

        if (_isGameRunning)
        {
            e.Cancel = true;

            var result = await ShowConfirmDialogAsync(
                title: "游戏仍在运行",
                message: "当前游戏仍在运行。\n\n关闭启动器前是否结束游戏？",
                yesText: "结束游戏",
                noText: "取消");

            if (result != MessageBoxResult.Yes)
                return;

            await EndRunningGameAsync("launcher closing");

            // A failed/in-progress stop must not close the launcher and its owned host.
            if (!CanCloseAfterGameStop()) return;

            await CompleteCloseAsync();
            return;
        }

        if (IsDedicatedServerRunning())
        {
            e.Cancel = true;

            var result = await ShowConfirmDialogAsync(
                title: "房主服务端仍在运行",
                message: "当前房主服务端仍在后台运行。\n\n关闭启动器会同时关闭本机服务端，其他玩家将无法继续连接你创建的服务器。\n\n是否关闭服务端并退出？",
                yesText: "关闭并退出",
                noText: "取消");

            if (result != MessageBoxResult.Yes)
                return;

            await CompleteCloseAsync();
            return;
        }
        e.Cancel = true;
        await CompleteCloseAsync();
    }

    private async Task CompleteCloseAsync()
    {
        if (_closingBackgroundTasks) return;
        // Info overlays have no cancellation contract. Keep the window usable until
        // the current overlay is dismissed, then allow a fresh close attempt.
        if (_dialogTcs != null) return;
        if (!StopDedicatedServer("launcher closing"))
        {
            await ShowInfoDialogAsync(L("服务端尚未停止", "Server Still Running"),
                L("本机服务端尚未确认停止。启动器将保持打开，请稍后重试。", "The local server has not confirmed its exit. The launcher will remain open; please retry."));
            return;
        }
        _closingBackgroundTasks = true;
        _windowOneShotCts.Cancel();
        StopLanTools();
        _gameLaunchService.SetDiagnosticIntent(RunIntent.LauncherClosed);
        StopHostBroadcast();
        // Await asynchronously while WPF can still drain queued dispatcher work.
        try
        {
            await Task.WhenAll(_gameMonitor.StopAsync(), _lanMonitor.StopAsync(),
                _hostBroadcast.StopAsync(), _hostDiscovery.StopAsync(), _presenceTask,
                _statusRefreshTask, _networkCheckTask, _windowLoadedTask);
            await _gameLaunchService.StopAsync();
        }
        catch (Exception ex) { LogService.Error("Launcher background shutdown failed: " + ex); }
        _windowOneShotCts.Dispose();
        _allowClose = true;
        // Queue the second Close even if every drain completed synchronously.
        _ = Dispatcher.BeginInvoke(new Action(Close));
    }

    protected override void OnClosed(EventArgs e)
    {
        StopLanTools();
        _gameLaunchService.SetDiagnosticIntent(RunIntent.LauncherClosed);
        CancelGameMonitor();
        _ = _gameLaunchService.StopAsync();
        _lanMonitor.Cancel();

        StopHostBroadcast();
        _hostDiscovery.Cancel();
        StopDedicatedServer("launcher closed");
        StopMusic();

        base.OnClosed(e);
    }

    private void LoadSettingsToUi()
    {
        _settings = _settingsService.Load();

        txtUsername.Text = _settings.Username;
        txtPassword.Password = "";
        chkRememberPassword.IsChecked = _settings.RememberPassword;
        string savedServer = string.IsNullOrWhiteSpace(_settings.ServerAddress) ? "" : _settings.ServerAddress.Trim();
        txtServer.Text = IsServerAddressEmptyOrDefault(savedServer) ? "" : savedServer;
        txtDetectedHostName.Text = "";
        RememberNonHostServerAddressIfUseful(_networkTarget);
        UpdateServerPlaceholderVisibility();
        cmbGameplayMode.SelectedValue = GameplayRuntime.Normalize(_settings.GameplayMode);
        fovSlider.Value = GameplayRuntime.NormalizeFov(_settings.FovScale);

        if (_gameLocatorService.IsValidGameDirectory(_settings.GameDirectory))
        {
            _gameDir = _settings.GameDirectory;
            LogService.Info($"Loaded saved game directory: {_gameDir}");
        }
        else
        {
            string? detectedDir = _gameLocatorService.TryAutoFindGameDirectory();
            if (!string.IsNullOrWhiteSpace(detectedDir))
            {
                _gameDir = detectedDir;
                _settings.GameDirectory = detectedDir;
                try
                {
                    _settingsService.Save(_settings);
                    LogService.Info($"Auto detected and saved game directory: {detectedDir}");
                }
                catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
                {
                    // Constructor-time persistence is optional; keep this valid path
                    // for the current run without opening an uninitialized dialog.
                    LogService.Error("Auto-detected game directory is available for this run but was not saved. Check settings folder write permission and retry. Failure type: " + ex.GetType().Name);
                }
            }
            else
            {
                _gameDir = "";
                LogService.Info("Game directory was not auto detected.");
            }
        }

        RestoreRememberedPassword();
        LogService.Info("Launcher ready.");
        LogService.Info($"Settings path: {_settingsService.SettingsPath}");
    }

    private void SaveSettingsFromUi()
    {
        _settings.Username = txtUsername.Text.Trim();
        _settings.Password = "";
        if (IsHostMode())
        {
            // 房主模式下服务器地址会被临时改成本机 Lan IP，不能把它保存成非房主默认地址。
            _settings.ServerAddress = _lastNonHostServerAddress;
        }
        else
        {
            RememberNonHostServerAddressIfUseful(_networkTarget);
            _settings.ServerAddress = string.IsNullOrWhiteSpace(_lastNonHostServerAddress)
                ? txtServer.Text.Trim()
                : _lastNonHostServerAddress;
        }
        _settings.GameDirectory = _gameDir;
        _settings.GameExecutable = GetSelectedGameExecutable();
        _settings.GameplayMode = GetSelectedGameplayMode();
        _settings.FovScale = GetSelectedFovScale();
        _settings.RememberPassword = chkRememberPassword.IsChecked==true;

        _settingsService.Save(_settings);
    }

    private string GetSelectedGameplayMode() => GameplayRuntime.Normalize(cmbGameplayMode.SelectedValue as string);

    private double GetSelectedFovScale() => GameplayRuntime.NormalizeFov(fovSlider.Value);

    private bool _restoringInteractiveSetting;
    private bool _interactiveSettingFailureShown;

    private bool TryPersistInteractiveSetting(Action apply, Action restore)
    {
        apply();
        try
        {
            // Persist only this already captured field change. Do not copy credentials,
            // recapture the server address, or rewind unrelated asynchronous UI state.
            _settingsService.Save(_settings);
            _interactiveSettingFailureShown = false;
            return true;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            _restoringInteractiveSetting = true;
            try { restore(); }
            finally { _restoringInteractiveSetting = false; }
            LogService.Error("Interactive setting write failed: " + ex.GetType().Name);
            // A drag can deliver several changes. Report once until a write succeeds;
            // do not steal an existing dialog or repeatedly interrupt the same failure.
            if (!_interactiveSettingFailureShown && _dialogTcs == null)
            {
                _interactiveSettingFailureShown = true;
                _ = ShowInfoDialogAsync(L("设置未保存", "Setting not saved"),
                    L("无法写入启动器设置，已恢复此项原值。请检查目录写入权限后重试。", "The launcher could not save this setting; its previous value was restored. Check folder write permission and retry."));
            }
            return false;
        }
    }

    private void FovSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if(txtFovValue!=null) txtFovValue.Text=GameplayRuntime.NormalizeFov(e.NewValue).ToString("0.00",System.Globalization.CultureInfo.InvariantCulture)+"×";
        if(!IsLoaded || App.PreviewMode || _restoringInteractiveSetting) return;
        double previous = _settings.FovScale;
        TryPersistInteractiveSetting(() => _settings.FovScale = GetSelectedFovScale(),
            () => { _settings.FovScale = previous; fovSlider.Value = e.OldValue; });
    }

    private void FovReset_Click(object sender,RoutedEventArgs e) => fovSlider.Value=1.2;

    private void GameplayMode_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if(!IsLoaded || App.PreviewMode || _restoringInteractiveSetting) return;
        string previous = _settings.GameplayMode;
        TryPersistInteractiveSetting(() => _settings.GameplayMode = GetSelectedGameplayMode(),
            () => { _settings.GameplayMode = previous; cmbGameplayMode.SelectedValue = GameplayRuntime.Normalize(previous); });
    }

    private string GetSelectedGameExecutable()
    {
        return "Blacklist_DX11_game.exe";
    }

    private string GetSelectedGameLabel()
    {
        return "DX11";
    }

    private bool IsHostMode()
    {
        // WPF 控件只能在 UI 线程访问。
        // UI 线程读取真实选择并刷新缓存；后台线程只读取缓存值。
        try
        {
            if (Dispatcher.CheckAccess() && cmbConnectionMode != null)
            {
                _isHostModeCached = cmbConnectionMode.SelectedIndex == 1;
                return _isHostModeCached;
            }
        }
        catch
        {
        }

        return _isHostModeCached;
    }

    private void UpdateConnectionModeUi()
    {
        bool hostMode = IsHostMode();

        if (hostMode)
        {
            txtDetectedHostName.Text = "";

            var local = GetLanLocalStatus();
            if (local.IsOk && local.Bind != null)
                SetServerAddressProgrammatically(local.Bind.IpAddress, null, resetStatus: false);
        }

        UpdateServerAddressInputState();
        UpdateServerPlaceholderVisibility();
        UpdateLaunchButtonAvailability();
    }

    private void SetServerAddressProgrammatically(string value, string? discoveredHostName = null, bool resetStatus = true)
    {
        try
        {
            _suppressServerTextChanged = true;
            txtServer.Text = value ?? "";
            txtDetectedHostName.Text = string.IsNullOrWhiteSpace(discoveredHostName) ? "" : $"房主：{discoveredHostName.Trim()}";
        }
        finally
        {
            _suppressServerTextChanged = false;
        }

        _serverAddressEditedAfterLastCheck = false;
        RestoreRememberedPassword();
        UpdateServerPlaceholderVisibility();

        if (resetStatus && !_statusRefreshRunning && !_isGameRunning && !_isGameStarting)
        {
            _lastServerReachable = false;
            SetServerStatus(Brushes.Goldenrod, "服务器连接失败", ServerStatusKind.ServerFailed);
        }
        else
        {
            UpdateLaunchButtonAvailability();
        }
    }


    private void UsernameTextBox_PreviewTextInput(object sender, TextCompositionEventArgs e)
    {
        e.Handled = !IsValidUsernameInput(e.Text);
    }

    private void UsernameTextBox_TextChanged(object sender, System.Windows.Controls.TextChangedEventArgs e)
    {
        if (_suppressUsernameTextChanged || txtUsername == null)
            return;

        ClearAutofillForChangedIdentity();
        string original = txtUsername.Text;
        string cleaned = FilterUsernameText(original);
        if (cleaned.Equals(original, StringComparison.Ordinal))
            return;

        int oldCaret = txtUsername.CaretIndex;
        int removedBeforeCaret = CountInvalidUsernameCharsBefore(original, oldCaret);
        int newCaret = Math.Max(0, Math.Min(cleaned.Length, oldCaret - removedBeforeCaret));

        try
        {
            _suppressUsernameTextChanged = true;
            txtUsername.Text = cleaned;
            txtUsername.CaretIndex = newCaret;
        }
        finally
        {
            _suppressUsernameTextChanged = false;
        }
    }

    private void UsernameTextBox_Pasting(object sender, DataObjectPastingEventArgs e)
    {
        if (!TryGetPastedText(e, out var text) || !IsValidUsernameInput(text))
            e.CancelCommand();
    }

    private void PasswordBox_PreviewTextInput(object sender, TextCompositionEventArgs e)
    {
        e.Handled = !IsValidPasswordInput(e.Text);
    }

    private void PasswordBox_Pasting(object sender, DataObjectPastingEventArgs e)
    {
        if (!TryGetPastedText(e, out var text) || !IsValidPasswordInput(text))
            e.CancelCommand();
    }

    private void ServerTextBox_PreviewTextInput(object sender, TextCompositionEventArgs e)
    {
        e.Handled = !IsValidServerAddressInput(e.Text);
    }

    private void ServerTextBox_Pasting(object sender, DataObjectPastingEventArgs e)
    {
        if (!TryGetPastedText(e, out var text) || !IsValidServerAddressInput(text))
            e.CancelCommand();
    }

    private static bool TryGetPastedText(DataObjectPastingEventArgs e, out string text)
    {
        text = string.Empty;
        if (!e.DataObject.GetDataPresent(DataFormats.UnicodeText))
            return false;

        text = e.DataObject.GetData(DataFormats.UnicodeText) as string ?? string.Empty;
        return true;
    }

    private static bool IsValidUsernameInput(string text)
    {
        if (string.IsNullOrEmpty(text))
            return true;

        foreach (char ch in text)
        {
            if (!IsValidUsernameChar(ch))
                return false;
        }

        return true;
    }

    private static bool IsValidUsernameChar(char ch)
    {
        return ch <= 127
            && !char.IsWhiteSpace(ch)
            && (char.IsLetterOrDigit(ch) || ch == '_' || ch == '-' || ch == '.');
    }

    private static string FilterUsernameText(string text)
    {
        if (string.IsNullOrEmpty(text))
            return string.Empty;

        var sb = new StringBuilder(text.Length);
        foreach (char ch in text)
        {
            if (IsValidUsernameChar(ch))
                sb.Append(ch);
        }
        return sb.ToString();
    }

    private static int CountInvalidUsernameCharsBefore(string text, int caretIndex)
    {
        int count = 0;
        int end = Math.Min(Math.Max(caretIndex, 0), text.Length);
        for (int i = 0; i < end; i++)
        {
            if (!IsValidUsernameChar(text[i]))
                count++;
        }
        return count;
    }

    private static bool IsValidPasswordInput(string text)
    {
        return PasswordPolicy.IsValidInput(text);
    }

    private static bool IsValidServerAddressInput(string text)
    {
        if (string.IsNullOrEmpty(text))
            return true;

        foreach (char ch in text)
        {
            if (!IsValidServerAddressChar(ch))
                return false;
        }

        return true;
    }

    private static bool IsValidServerAddressChar(char ch)
    {
        // 服务器地址只允许英文域名/IP/端口/协议所需字符，禁止中文、空格和全角符号。
        return ch <= 127
            && !char.IsWhiteSpace(ch)
            && (char.IsLetterOrDigit(ch) || ch == '.' || ch == '-' || ch == ':' || ch == '/');
    }

    private static string FilterServerAddressText(string text)
    {
        if (string.IsNullOrEmpty(text))
            return string.Empty;

        var sb = new StringBuilder(text.Length);
        foreach (char ch in text)
        {
            if (IsValidServerAddressChar(ch))
                sb.Append(ch);
        }
        return sb.ToString();
    }

    private static int CountInvalidServerAddressCharsBefore(string text, int caretIndex)
    {
        int count = 0;
        int end = Math.Min(Math.Max(caretIndex, 0), text.Length);
        for (int i = 0; i < end; i++)
        {
            if (!IsValidServerAddressChar(text[i]))
                count++;
        }
        return count;
    }

    private void ServerTextBox_TextChanged(object sender, System.Windows.Controls.TextChangedEventArgs e)
    {
        if (txtDetectedHostName == null || txtServerStatus == null || serverStatusLight == null || btnLaunch == null)
            return;

        if (!_suppressServerTextChanged && txtServer != null)
        {
            string original = txtServer.Text;
            string cleaned = FilterServerAddressText(original);
            if (!cleaned.Equals(original, StringComparison.Ordinal))
            {
                int oldCaret = txtServer.CaretIndex;
                int removedBeforeCaret = CountInvalidServerAddressCharsBefore(original, oldCaret);
                int newCaret = Math.Max(0, Math.Min(cleaned.Length, oldCaret - removedBeforeCaret));

                try
                {
                    _suppressServerTextChanged = true;
                    txtServer.Text = cleaned;
                    txtServer.CaretIndex = newCaret;
                }
                finally
                {
                    _suppressServerTextChanged = false;
                }
            }
        }

        UpdateServerPlaceholderVisibility();

        ClearAutofillForChangedIdentity();
        _networkTarget=txtServer?.Text?.Trim() ?? "";
        if (_suppressServerTextChanged)
            return;

        txtDetectedHostName.Text = "";
        _serverAddressEditedAfterLastCheck = true;
        if (!IsHostMode())
            RememberNonHostServerAddressIfUseful(_networkTarget);

        if (!_isGameRunning && !_isGameStarting)
        {
            _lastServerReachable = false;
            SetServerStatus(Brushes.Goldenrod, "服务器连接失败", ServerStatusKind.ServerFailed);
        }
    }

    private void ServerTextBox_GotKeyboardFocus(object sender, KeyboardFocusChangedEventArgs e)
    {
        UpdateServerPlaceholderVisibility();
    }

    private void ServerTextBox_LostKeyboardFocus(object sender, KeyboardFocusChangedEventArgs e)
    {
        UpdateServerPlaceholderVisibility();
    }

    private void UpdateServerAddressInputState(bool? lanReadyOverride = null)
    {
        if(txtServer==null || txtServerPlaceholder==null) return;
        txtServer.IsEnabled=!IsHostMode() && !_isBusy && !_isGameRunning && !_isGameStarting;
        txtServerPlaceholder.Text=L(ServerPlaceholderNormal,"Enter server IPv4/domain; discovery is optional");
    }

    private void UpdateServerPlaceholderVisibility()
    {
        if (txtServer == null || txtServerPlaceholder == null || txtDetectedHostName == null || bdDetectedHostName == null)
            return;

        bool showPlaceholder = !IsHostMode()
            && string.IsNullOrWhiteSpace(txtServer.Text)
            && (!txtServer.IsKeyboardFocused || !txtServer.IsEnabled);

        txtServerPlaceholder.Visibility = showPlaceholder ? Visibility.Visible : Visibility.Collapsed;
        bdDetectedHostName.Visibility = !IsHostMode() && !string.IsNullOrWhiteSpace(txtDetectedHostName.Text)
            ? Visibility.Visible
            : Visibility.Collapsed;
    }

    private bool CanLaunchGame()
    {
        if (_isGameRunning)
            return !_isEndingGame;

        if (_isGameStarting || _statusRefreshRunning || _isBusy)
            return false;

        // 启动按钮默认保持可点击。
        // 真正的网络校验仍然放在启动游戏前执行：
        // 如果 Lan / 服务器连接异常，会弹出对应提示并取消启动。
        return true;
    }

    private void UpdateLaunchButtonAvailability()
    {
        if (btnLaunch == null)
            return;

        if (_isGameRunning || _isGameStarting)
            return;

        btnLaunch.IsEnabled = CanLaunchGame();
    }

    private void ClearServerAddressIfCurrentLanAddress()
    {
        string current = txtServer.Text.Trim();
        if (!IsLocalLanServerAddress(current))
        {
            txtDetectedHostName.Text = "";
            UpdateServerPlaceholderVisibility();
            return;
        }

        if (!string.IsNullOrWhiteSpace(_lastNonHostServerAddress))
        {
            SetServerAddressProgrammatically(_lastNonHostServerAddress, null);
        }
        else
        {
            SetServerAddressProgrammatically("", null);
        }
    }

    private void RememberNonHostServerAddressIfUseful(string? server)
    {
        string value = server?.Trim() ?? "";
        if (IsServerAddressEmptyOrDefault(value))
            return;

        if (IsLocalLanServerAddress(value))
            return;

        _lastNonHostServerAddress = value;
    }

    private bool IsLocalLanServerAddress(string? server)
    {
        if (string.IsNullOrWhiteSpace(server))
            return false;

        var local = GetLanLocalStatus();
        if (local.Bind == null)
            return false;

        string currentHost = NormalizeServerHost(server);
        return currentHost.Equals(local.Bind.IpAddress, StringComparison.OrdinalIgnoreCase);
    }

    private async void ConnectionMode_SelectionChanged(object sender, System.Windows.Controls.SelectionChangedEventArgs e)
    {
        if (_isChangingConnectionMode || !IsLoaded)
            return;

        _isHostModeCached = cmbConnectionMode.SelectedIndex == 1;
        bool wantsHostMode = _isHostModeCached;

        if (wantsHostMode)
        {
            RememberNonHostServerAddressIfUseful(_networkTarget);
            string requestedHostName = txtUsername.Text.Trim();
            if (string.IsNullOrWhiteSpace(requestedHostName))
            {
                RevertConnectionModeSelection(0);
                UpdateConnectionModeUi();
                await ShowInfoDialogAsync(
                    L("请先填写账号名", "Username Required"),
                    L("房主模式会使用启动器账号名作为房主显示名称。\n\n请先填写账号名，再切换到房主模式。", "Host Mode uses your username as the displayed host name.\n\nPlease enter a username before switching to Host Mode."));
                return;
            }

            var result = await ShowConfirmDialogAsync(
                title: L("切换为房主模式", "Switch to Host Mode"),
                message: L("启动器将使用你的 本机网卡地址启动本机服务端，其他玩家可通过自动发现或手动输入你的 IP 加入。\n\n是否继续？", "The launcher will start the local server using your LAN network address. Other players can join through host discovery or by entering your IP manually.\n\nContinue?"),
                yesText: L("确认切换", "Switch"),
                noText: L("取消", "Cancel"));

            if (result != MessageBoxResult.Yes)
            {
                RevertConnectionModeSelection(0);
                UpdateConnectionModeUi();
                return;
            }

            bool ok = await StartHostModeServerFromUiAsync();
            if (!ok)
            {
                RevertConnectionModeSelection(IsDedicatedServerRunning() ? 1 : 0);
                UpdateConnectionModeUi();
                await RefreshServerStatusAsync(showFailureDialog: false);
                return;
            }

            UpdateConnectionModeUi();
            await RefreshServerStatusAsync(showFailureDialog: false);
            return;
        }

        if (IsDedicatedServerRunning())
        {
            var result = await ShowConfirmDialogAsync(
                title: L("切换联机模式", "Switch Connection Mode"),
                message: L("当前房主服务端正在后台运行。\n\n切换到非房主模式后，启动器将关闭本机服务端，其他玩家将无法继续连接你创建的服务器。\n\n是否继续切换？", "The host server is currently running in the background.\n\nSwitching to Client Mode will stop the local server, and other players will no longer be able to connect to your server.\n\nContinue?"),
                yesText: L("继续切换", "Switch"),
                noText: L("取消", "Cancel"));

            if (result == MessageBoxResult.Yes)
            {
                if (!StopDedicatedServer("switch to client mode"))
                {
                    RevertConnectionModeSelection(1);
                    UpdateConnectionModeUi();
                    await ShowInfoDialogAsync(L("服务端尚未停止", "Server Still Running"),
                        L("本机服务端尚未确认停止，已保留房主模式。请稍后重试。", "The local server has not confirmed its exit. Host Mode was retained; please retry."));
                    return;
                }
                StopHostBroadcast();
                ClearServerAddressIfCurrentLanAddress();
            }
            else
            {
                RevertConnectionModeSelection(1);
                UpdateConnectionModeUi();
                return;
            }
        }

        StopHostBroadcast();
        ClearServerAddressIfCurrentLanAddress();
        UpdateConnectionModeUi();
        await RefreshServerStatusAsync(showFailureDialog: false);
    }

    private void RevertConnectionModeSelection(int selectedIndex)
    {
        try
        {
            _isChangingConnectionMode = true;
            cmbConnectionMode.SelectedIndex = selectedIndex;
            _isHostModeCached = selectedIndex == 1;
        }
        finally
        {
            _isChangingConnectionMode = false;
        }
    }

    private async Task<bool> StartHostModeServerFromUiAsync()
    {
        try
        {
            SetBusy(true, L("启动服务端...", "Starting server..."));

            var local = GetLanLocalStatus();
            if (!local.IsOk || local.Bind == null) throw new IOException(string.IsNullOrEmpty(local.Error) ? "未找到可用本机 IPv4，请检查网卡或手动选择。" : local.Error);

            string hostName = txtUsername.Text.Trim();
            if (string.IsNullOrWhiteSpace(hostName))
                throw new Exception(L("请先填写账号名。房主广播会使用这个账号名作为房主显示名称。", "Please enter a username first. Host discovery uses it as the displayed host name."));

            if (!IsValidUsernameInput(hostName))
                throw new Exception(L("账号只能使用英文、数字、下划线、短横线或点，不能包含中文或空格。", "Username can only contain letters, numbers, underscores, hyphens, or dots. Chinese characters and spaces are not allowed."));

            string server = await EnsureHostServerReadyAsync(local.Bind.IpAddress, hostName);
            SetServerAddressProgrammatically(server, null, resetStatus: false);
            SaveSettingsFromUi();
            return true;
        }
        catch (Exception ex)
        {
            LogService.Error($"Failed to start host mode server: {ex}");
            StopHostBroadcast();
            StopDedicatedServer("host mode start failed");
            await ShowInfoDialogAsync(L("房主模式启动失败", "Host Mode Failed"), ex.Message);
            return false;
        }
        finally
        {
            SetBusy(false);
        }
    }

    private void AppendUiLog(string message)
    {
        LogService.Info(message);
    }

    private void AppendUiError(string message)
    {
        LogService.Error(message);
    }

    private void SetBusy(bool busy, string? buttonText = null)
    {
        _isBusy = busy;
        bdDetectedHostName.IsEnabled = !busy;
        cmbBindAdapter.IsEnabled=!busy && !_isGameRunning && !_isGameStarting && !IsDedicatedServerRunning();
        btnCheckNetwork.IsEnabled = !busy && !_statusRefreshRunning && !_isGameRunning && !_isGameStarting;
        chkRememberPassword.IsEnabled = !busy && !_isGameRunning && !_isGameStarting;
        cmbGameplayMode.IsEnabled = !busy && !_isGameRunning && !_isGameStarting;
        fovSlider.IsEnabled = btnFovReset.IsEnabled = !busy && !_isGameRunning && !_isGameStarting;
        cmbConnectionMode.IsEnabled = !busy && !_isGameRunning && !_isGameStarting;
        UpdateServerAddressInputState(_lastLanLocalReady);
        txtUsername.IsEnabled = !busy && !_isGameRunning && !_isGameStarting;
        txtPassword.IsEnabled = !busy && !_isGameRunning && !_isGameStarting;

        if (_isGameRunning)
        {
            ApplyLaunchButtonStyle(danger: true);
            btnLaunch.IsEnabled = !_isEndingGame;
            btnLaunch.Content = _isEndingGame ? L("正在结束...", "Ending...") : L("结束游戏", "End Game");
            return;
        }

        if (_isGameStarting)
        {
            ApplyLaunchButtonStyle(danger: false);
            btnLaunch.IsEnabled = !_isEndingGame;
            btnLaunch.Content = L("重新启动", "Restart");
            return;
        }

        ApplyLaunchButtonStyle(danger: false);
        btnLaunch.IsEnabled = !busy && CanLaunchGame();
        btnLaunch.Content = buttonText != null ? LocalizeBusyText(buttonText) : (busy ? L("处理中...", "Working...") : L("启动游戏", "Start Game"));
    }

    private void ApplyLaunchButtonStyle(bool danger)
    {
        try
        {
            btnLaunch.Style = (Style)FindResource(danger ? "DangerButton" : "PrimaryButton");
        }
        catch
        {
            btnLaunch.Background = danger
                ? new SolidColorBrush(Color.FromRgb(210, 63, 63))
                : new SolidColorBrush(Color.FromRgb(36, 198, 123));
            btnLaunch.Foreground = danger ? Brushes.White : new SolidColorBrush(Color.FromRgb(2, 17, 8));
        }
    }

    private void SetGameStartingState(bool starting, string? buttonText = null)
    {
        _isGameStarting = starting;
        if (starting) _unconfirmedLaunchSession = null;
        SetBusy(false, buttonText ?? L("重新启动", "Restart"));
    }

    private void SetGameRunningState(bool running)
    {
        _isGameRunning = running;

        if (running)
        {
            PauseMusicForGameSession();
            _isGameStarting = false;
        }
        else
        {
            _runningGameProcessName = null;
            _unconfirmedLaunchSession = null;
            _isEndingGame = false;
            _isGameStarting = false;
        }

        SetBusy(false);
    }

    private void BringLauncherToFront(string reason)
    {
        try
        {
            if (WindowState == WindowState.Minimized)
                WindowState = WindowState.Normal;

            Show();
            Activate();

            // 临时置顶再取消，尽量把启动器从游戏后面拉回桌面。
            Topmost = true;
            Topmost = false;
            Focus();

            LogService.Info($"Launcher brought to front: {reason}");
        }
        catch (Exception ex)
        {
            LogService.Error($"Failed to bring launcher to front: {ex}");
        }
    }

    private string? AskUserToSelectGameDirectory()
    {
        var dialog = new OpenFileDialog
        {
            Title = "请选择 DX11：Blacklist_DX11_game.exe",
            Filter = "Splinter Cell Blacklist|Blacklist_DX11_game.exe|所有文件|*.*"
        };

        if (dialog.ShowDialog() != true)
            return null;

        string fileName = Path.GetFileName(dialog.FileName);
        if (!fileName.Equals("Blacklist_DX11_game.exe", StringComparison.OrdinalIgnoreCase))
        {
            throw new Exception("SCBL LAN 1.0 仅支持 DX11，请选择 Blacklist_DX11_game.exe；DX9 不再受支持。");
        }
        return Path.GetDirectoryName(dialog.FileName) ?? "";
    }

    private string ResolveGameDirectoryOrAsk()
    {
        if (_gameLocatorService.IsValidGameDirectory(_gameDir))
            return _gameDir;

        if (_gameLocatorService.IsValidGameDirectory(_settings.GameDirectory))
        {
            _gameDir = _settings.GameDirectory;
            return _gameDir;
        }

        string? detectedDir = _gameLocatorService.TryAutoFindGameDirectory();
        if (!string.IsNullOrWhiteSpace(detectedDir))
        {
            _gameDir = detectedDir;
            SaveSettingsFromUi();
            return _gameDir;
        }

        string? selectedDir = AskUserToSelectGameDirectory();
        if (string.IsNullOrWhiteSpace(selectedDir))
            throw new Exception("未选择游戏主程序，已取消启动。");

        _gameDir = selectedDir;
        SaveSettingsFromUi();
        return _gameDir;
    }

    private void DetectNetworkSilently()
    {
        try
        {
            DetectNetwork(showDetails: false);
        }
        catch
        {
            // Silent startup network check failure should not block the UI.
        }
    }

    private BindIpCandidate? DetectNetwork(bool showDetails)
    {
        var selected=_networkService.Select(_settings.BindOverride,_networkTarget,_isHostModeCached);
        if(selected!=null) _settings.LastBindIp=selected.IpAddress;
        return selected;
    }

    private bool ValidateServerAddressForAction(string server, bool showFailureDialog, out string message)
    {
        message = string.Empty;

        if (string.IsNullOrWhiteSpace(server))
        {
            message = "请先填写服务器地址。";
            if (showFailureDialog)
                _ = ShowInfoDialogAsync("未填写服务器地址", message);
            return false;
        }

        if (!IsValidServerAddressText(server, out message))
        {
            if (showFailureDialog)
                _ = ShowInfoDialogAsync("服务器地址格式不正确", message);
            return false;
        }

        return true;
    }

    private static bool IsValidServerAddressText(string server,out string message)
    {
        try { ScblLan.ServerAddress.Parse(server);message="";return true; }
        catch(Exception e) {message=e.Message;return false;}
    }

    private static bool IsValidHostNameOrIpv4(string host, out string message)
    {
        message = string.Empty;
        host = (host ?? string.Empty).Trim();

        if (host.Length == 0)
        {
            message = "服务器地址不能为空。";
            return false;
        }

        if (IPAddress.TryParse(host, out var ip))
        {
            if (ip.AddressFamily == AddressFamily.InterNetwork)
                return true;

            message = "当前只支持 IPv4 地址或域名。";
            return false;
        }

        if (host.Equals("localhost", StringComparison.OrdinalIgnoreCase))
            return true;

        if (host.Length > 253 || host.StartsWith('.') || host.EndsWith('.'))
        {
            message = "域名格式不正确。";
            return false;
        }

        string[] labels = host.Split('.');
        foreach (string label in labels)
        {
            if (label.Length < 1 || label.Length > 63)
            {
                message = "域名格式不正确。";
                return false;
            }

            if (label.StartsWith('-') || label.EndsWith('-'))
            {
                message = "域名每段不能以短横线开头或结尾。";
                return false;
            }

            foreach (char ch in label)
            {
                if (!(char.IsLetterOrDigit(ch) || ch == '-'))
                {
                    message = "域名只能包含英文、数字、点和短横线。";
                    return false;
                }
            }
        }

        return true;
    }

    private bool HasServerAddressChangedSinceCheckStarted(string checkedServer)
    {
        string current = txtServer.Text.Trim();
        return !current.Equals(checkedServer, StringComparison.OrdinalIgnoreCase);
    }

    private void CancelStaleServerCheckIfNeeded(string checkedServer)
    {
        if (!HasServerAddressChangedSinceCheckStarted(checkedServer))
            return;

        _lastServerReachable = false;
        SetServerStatus(Brushes.Goldenrod, "服务器连接失败", ServerStatusKind.ServerFailed);
    }

    private Task<bool> EnsureCurrentHostHeartbeatOrFailAsync(string server,bool showFailureDialog)
    {
        // Broadcast is optional on routed overlays; capability/API checks remain mandatory.
        return Task.FromResult(true);
    }

    private Task RefreshServerStatusAsync(bool keepButtonDisabled = false, bool showFailureDialog = false)
    {
        if (_closingBackgroundTasks || _statusRefreshRunning) return Task.CompletedTask;
        return _statusRefreshTask = RefreshServerStatusCoreAsync(keepButtonDisabled, showFailureDialog);
    }

    private async Task RefreshServerStatusCoreAsync(bool keepButtonDisabled, bool showFailureDialog)
    {

        string server = txtServer.Text.Trim();
        string checkedServer = server;

        try
        {
            _statusRefreshRunning = true;
            btnCheckNetwork.IsEnabled = false;
            btnCheckNetwork.Content = L("检测中...", "Checking...");
            SetServerStatus(Brushes.DeepSkyBlue, "检测中...", ServerStatusKind.Checking);

            var local = GetLanLocalStatus();
            if (!local.IsOk || local.Bind == null)
            {
                _lastLanLocalReady=false; _lastServerReachable=false;
                UpdateServerAddressInputState(false);
                SetServerStatus(Brushes.Goldenrod,"未选择可用网卡",ServerStatusKind.LocalAbnormal);
                if(showFailureDialog) await ShowInfoDialogAsync("本机网络未就绪",string.IsNullOrEmpty(local.Error)?"请选择本机网卡；不要求安装特定组网软件。":local.Error);
                return;
            }

            _lastLanLocalReady = true;
            UpdateServerAddressInputState(true);

            if (string.IsNullOrWhiteSpace(server))
            {
                _lastServerReachable = false;
                _serverAddressEditedAfterLastCheck = true;
                SetServerStatus(Brushes.Goldenrod, "服务器连接失败", ServerStatusKind.ServerFailed);
                if (showFailureDialog)
                    await ShowInfoDialogAsync(L("未填写服务器地址", "Server Address Required"), L("请等待自动发现房主，或手动输入房主 IP / 域名。", "Please wait for host discovery or enter the host IP/domain manually."));
                return;
            }

            if (!IsValidServerAddressText(server, out string addressFormatMessage))
            {
                _lastServerReachable = false;
                SetServerStatus(Brushes.Goldenrod, "服务器连接失败", ServerStatusKind.ServerFailed);
                if (showFailureDialog)
                    await ShowInfoDialogAsync("服务器地址格式不正确", addressFormatMessage);
                return;
            }

            if (!await EnsureCurrentHostHeartbeatOrFailAsync(server, showFailureDialog))
                return;
            if (_closingBackgroundTasks) return;
            _windowOneShotCts.Token.ThrowIfCancellationRequested();

            _serverAddressEditedAfterLastCheck = false;
            checkedServer = server;

            if (HasServerAddressChangedSinceCheckStarted(checkedServer))
            {
                CancelStaleServerCheckIfNeeded(checkedServer);
                return;
            }

            var broadcastIntent = _hostBroadcastIntent;
            string checkedBind = _settings.BindOverride;
            bool mainOk = await _authService.CheckServerReachableAsync(server, checkedBind, _windowOneShotCts.Token);
            if (_closingBackgroundTasks) return;
            _windowOneShotCts.Token.ThrowIfCancellationRequested();
            if (HasServerAddressChangedSinceCheckStarted(checkedServer))
            {
                CancelStaleServerCheckIfNeeded(checkedServer);
                return;
            }

            if (!mainOk)
            {
                _lastServerReachable = false;
                SetServerStatus(Brushes.Goldenrod, "服务器连接失败", ServerStatusKind.ServerFailed);

                if (showFailureDialog)
                    await ShowInfoDialogAsync(L("服务器连接失败", "Server Failed"), L("请检查以下内容：\n1. 服务器地址或 IP 是否填写正确\n2. 服务端是否正在运行\n3. 当前网络是否正常", "Please check:\n1. Whether the server address/IP is correct\n2. Whether the server is running\n3. Whether the current network is normal"));

                return;
            }

            TryResumeHostBroadcast(broadcastIntent, checkedServer, checkedBind);
            _lastServerReachable = true;
            SetServerStatus(Brushes.LimeGreen, "服务器连接正常", ServerStatusKind.Normal);
        }
        catch (OperationCanceledException) when (_windowOneShotCts.IsCancellationRequested) { }
        catch (Exception ex)
        {
            LogService.Error($"Network check failed: {ex}");
            if (_closingBackgroundTasks) return;
            _lastServerReachable = false;
            SetServerStatus(Brushes.Goldenrod, "服务器连接失败", ServerStatusKind.ServerFailed);

            if (showFailureDialog)
                await ShowInfoDialogAsync(L("服务器连接失败", "Server Failed"), L("请检查以下内容：\n1. 服务器地址或 IP 是否填写正确\n2. 服务端是否正在运行\n3. 当前网络是否正常", "Please check:\n1. Whether the server address/IP is correct\n2. Whether the server is running\n3. Whether the current network is normal"));
        }
        finally
        {
            _statusRefreshRunning = false;
            if (!_closingBackgroundTasks)
            {
                btnCheckNetwork.Content = L("检测网络", "Check Network");
                if (!keepButtonDisabled && !_networkCheckRunning)
                    btnCheckNetwork.IsEnabled = !_isGameRunning && !_isGameStarting;
                UpdateServerAddressInputState(_lastLanLocalReady);
                UpdateServerPlaceholderVisibility();
                UpdateLaunchButtonAvailability();
            }
        }
    }

    private async void CheckNetworkButton_Click(object sender, RoutedEventArgs e)
    {
        if (_closingBackgroundTasks || _networkCheckRunning || _statusRefreshRunning || _isGameRunning || _isGameStarting)
            return;
        _networkCheckRunning = true;
        await (_networkCheckTask = CheckNetworkAsync());
    }

    private async Task CheckNetworkAsync()
    {
        btnCheckNetwork.IsEnabled = false;
        try
        {
            await RefreshServerStatusAsync(keepButtonDisabled: true, showFailureDialog: true);
            if (_closingBackgroundTasks) return;
            // Prevent repeated clicks when the server responds quickly.
            await Task.Delay(3000, _windowOneShotCts.Token);
        }
        catch (OperationCanceledException) when (_windowOneShotCts.IsCancellationRequested) { }
        catch (Exception ex) { LogService.Error("Network button task failed: " + ex); }
        finally
        {
            _networkCheckRunning = false;
            if (!_closingBackgroundTasks && !_isGameRunning && !_isGameStarting)
                btnCheckNetwork.IsEnabled = true;
        }
    }

    private void StartLanMonitor()
    {
        if (_closingBackgroundTasks) return;
        _lanMonitor.Start(async token =>
        {
            while (!token.IsCancellationRequested)
            {
                try
                {
                    await Dispatcher.InvokeAsync(() => { if (!token.IsCancellationRequested) ApplyLanMonitorStatus(); });
                    await Task.Delay(2000, token);
                }
                catch (TaskCanceledException)
                {
                    break;
                }
                catch (Exception ex)
                {
                    LogService.Error($"Lan monitor failed: {ex}");
                    try
                    {
                        await Task.Delay(2000, token);
                    }
                    catch
                    {
                        break;
                    }
                }
            }
        });
    }

    private void QueueAutomaticServerCheckAfterLanOnline()
    {
        if (_statusRefreshRunning || _isGameRunning || _isGameStarting)
            return;

        if (string.IsNullOrWhiteSpace(txtServer?.Text?.Trim()))
            return;

        // 玩家正在手动编辑服务器地址时，不要拿半截 IP/域名自动检测，
        // 否则界面会在输入过程中跳到“服务器连接失败”。
        if (_serverAddressEditedAfterLastCheck && !IsHostMode())
            return;

        var now = DateTime.UtcNow;
        if (now - _lastAutoServerCheckUtc < TimeSpan.FromSeconds(5))
            return;

        _lastAutoServerCheckUtc = now;
        _ = RefreshServerStatusAsync(keepButtonDisabled: false, showFailureDialog: false);
    }

    private void ApplyLanMonitorStatus()
    {
        if (_statusRefreshRunning || _isGameRunning || _isGameStarting || _isBusy) return;
        var local=GetLanLocalStatus();
        
        _lastLanLocalReady=local.IsOk;
        if(!local.IsOk) { _lastServerReachable=false;PauseHostBroadcast();SetServerStatus(Brushes.Goldenrod,"请选择可用网卡",ServerStatusKind.LocalAbnormal); }
        UpdateServerAddressInputState();
        if(local.IsOk) QueueAutomaticServerCheckAfterLanOnline();
        UpdateLaunchButtonAvailability();
    }

    private LanLocalStatus GetLanLocalStatus()
    {
        try {return new LanLocalStatus {Bind=DetectNetwork(false)};}
        catch(Exception e) {return new LanLocalStatus {Error=e.Message};}
    }

    private void SetServerStatus(Brush brush, string text, ServerStatusKind kind)
    {
        _serverStatusKind = kind;
        serverStatusLight.Fill = brush;
        txtServerStatus.Text = LocalizeStatusText(text);
        UpdateLaunchButtonAvailability();
    }

    private async void LaunchButton_Click(object sender, RoutedEventArgs e)
    {
        if(_isBusy) return;
        if (_isGameStarting)
        {
            await RestartGameDuringStartupAsync();
            return;
        }

        if (_isGameRunning)
        {
            await EndRunningGameWithConfirmAsync();
            return;
        }

        if (!CanLaunchGame())
        {
            await RefreshServerStatusAsync(keepButtonDisabled: true, showFailureDialog: true);
            if (!CanLaunchGame())
                return;
        }

        await RunLaunchFlowAsync();
    }


    private async Task RestartGameDuringStartupAsync()
    {
        if (!_isGameStarting)
            return;

        var result = await ShowConfirmDialogAsync(
            title: "重新启动游戏",
            message: "当前游戏仍在启动中，尚未检测到稳定运行的游戏进程。\n\n如果游戏窗口没有正常打开、弹错或已经闪退，可以取消本次等待并重新启动。\n\n是否重新启动？",
            yesText: "重新启动",
            noText: "取消");

        if (result != MessageBoxResult.Yes)
            return;

        string? processName = _runningGameProcessName;
        LogService.Info($"User requested restart while game is starting. Process={processName ?? "unknown"}");
        _gameLaunchService.SetDiagnosticIntent(RunIntent.UserRestart);

        CancelGameMonitor();
        await _gameLaunchService.StopAsync();

        if (!string.IsNullOrWhiteSpace(processName))
            KillGameProcessesByName(processName, "restart while game starting");

        SetGameRunningState(false);
        await Task.Delay(500);
        await RunLaunchFlowAsync();
    }

    private void KillGameProcessesByName(string processName,string reason)
    {
        foreach(var p in Process.GetProcessesByName(processName))
        { using(p) {try {if(IsOwnedGameProcess(p)) p.Kill(entireProcessTree:false);}catch(Exception e){LogService.Error(e.Message);} } }
    }

    private async Task EndRunningGameWithConfirmAsync()
    {
        if (!_isGameRunning)
            return;

        var result = await ShowConfirmDialogAsync(
            title: "结束游戏",
            message: "当前游戏仍在运行。\n\n是否结束游戏？",
            yesText: "结束游戏",
            noText: "取消");

        if (result != MessageBoxResult.Yes)
            return;

        await EndRunningGameAsync("user clicked end game");
    }

    private async Task EndRunningGameAsync(string reason)
    {
        if (_isEndingGame) return;
        _isEndingGame = true;
        _gameLaunchService.SetDiagnosticIntent(RunIntent.UserEnd);
        string? processName = _runningGameProcessName;
        CancelGameMonitor();
        SetBusy(false);
        var handles = new List<Process>();
        var owned = new List<Process>();
        bool StillAlive(Process p) {try {return !p.HasExited;}catch{return true;}}
        try
        {
            if (_unconfirmedLaunchSession is Guid unconfirmed)
            {
                if (!await _gameLaunchService.EndUnconfirmedLaunchAsync(unconfirmed))
                    throw new IOException("本次未确认启动的身份已变化，请确认进程状态后重试。");
                if (_unconfirmedLaunchSession == unconfirmed)
                {
                    _unconfirmedLaunchSession = null;
                    SetGameRunningState(false);
                }
                return; // Never fall through to the legacy name-based path.
            }
            if (!string.IsNullOrWhiteSpace(processName))
            {
                handles.AddRange(Process.GetProcessesByName(processName));
                foreach (var p in handles)
                {
                    int id=p.Id;
                    try
                    {
                        if (!IsOwnedGameProcess(p)) continue;
                        owned.Add(p);
                        LogService.Info($"Stopping owned game PID={id}, reason={reason}");
                        if (!p.HasExited) p.Kill(entireProcessTree:false);
                        using var timeout=new CancellationTokenSource(TimeSpan.FromSeconds(5));
                        await p.WaitForExitAsync(timeout.Token);
                    }
                    catch (Exception ex)
                    {
                        // Never fetch ProcessName after exit; that can itself throw.
                        LogService.Error($"Stop observation PID={id}: {ex.GetType().Name}");
                    }
                }
                var deadline=Stopwatch.StartNew();
                while ((owned.Any(StillAlive) || IsProcessNameRunning(processName)) && deadline.Elapsed<TimeSpan.FromSeconds(3))
                    await Task.Delay(100);
                if (owned.Any(StillAlive) || IsProcessNameRunning(processName))
                    throw new IOException("本次游戏进程仍在运行或无法确认退出，请重试或手动关闭。");
            }
            // Exit is an observed process state, independent of optional cleanup.
            try {await _gameLaunchService.StopAsync();}
            catch(Exception ex) {LogService.Error($"Game ended; configuration cleanup warning: {ex.GetType().Name}");}
            SetGameRunningState(false);
            LogService.Info("Owned game exit confirmed; launcher returned to idle.");
        }
        catch (Exception ex)
        {
            LogService.Error($"Failed while ending game: {ex}");
            await ShowInfoDialogAsync("结束失败",ex.Message);
        }
        finally
        {
            foreach(var p in handles) p.Dispose();
            _isEndingGame = false;
            SetBusy(false);
        }
    }

    private bool CanCloseAfterGameStop() => !_isGameRunning && !_isGameStarting && !_isEndingGame;

    private Task<bool> ConfirmNetworkRiskBeforeLaunchAsync() => Task.FromResult(true);

    private static bool ValidateGameRuntimeAfterStart(string processName, string expectedGamePath, string expectedHookPath, out string? warning)
    {
        warning = null;
        expectedGamePath = Path.GetFullPath(expectedGamePath);
        expectedHookPath = Path.GetFullPath(expectedHookPath);

        try
        {
            var processes = Process.GetProcessesByName(processName);
            if (processes.Length == 0)
            {
                warning = "游戏进程已经稳定出现过，但校验时没有找到对应进程。";
                return false;
            }

            var actualPaths = new List<string>();
            foreach (var process in processes)
            {
                try
                {
                    string? actualPath = process.MainModule?.FileName;
                    if (!string.IsNullOrWhiteSpace(actualPath))
                        actualPaths.Add(actualPath);

                    if (string.IsNullOrWhiteSpace(actualPath) ||
                        !Path.GetFullPath(actualPath).Equals(expectedGamePath, StringComparison.OrdinalIgnoreCase))
                    {
                        continue;
                    }

                    bool hookLoaded = false;
                    string? loadedHookPath = null;
                    foreach (ProcessModule module in process.Modules)
                    {
                        string moduleName = module.ModuleName ?? "";
                        if (!moduleName.Equals("uplay_r1_loader.dll", StringComparison.OrdinalIgnoreCase))
                            continue;

                        hookLoaded = true;
                        loadedHookPath = module.FileName;
                        break;
                    }

                    if (!hookLoaded)
                    {
                        warning =
                            "游戏已经启动，但没有检测到游戏进程加载 uplay_r1_loader.dll。\n\n" +
                            "这通常会导致游戏没有真正接入国服联机环境。\n\n" +
                            "请检查杀软是否拦截 DLL、是否从本启动器启动游戏、游戏目录是否正确。";
                        LogService.Error("Runtime validation failed: uplay_r1_loader.dll was not loaded by game process.");
                        return false;
                    }

                    if (string.IsNullOrWhiteSpace(loadedHookPath) ||
                        !Path.GetFullPath(loadedHookPath).Equals(expectedHookPath, StringComparison.OrdinalIgnoreCase))
                    {
                        warning =
                            "游戏已经启动，但加载的 uplay_r1_loader.dll 不是当前游戏目录下的国服联机 DLL。\n\n" +
                            $"期望 DLL：\n{expectedHookPath}\n\n" +
                            $"实际 DLL：\n{loadedHookPath ?? "<未知>"}\n\n" +
                            "请关闭原版 5th Echelon 启动器、杀软拦截或其它会覆盖 DLL 的程序后重试。";
                        LogService.Error($"Runtime validation failed: loaded hook path mismatch. Expected={expectedHookPath}, Actual={loadedHookPath}");
                        return false;
                    }

                    LogService.Info($"Runtime validation succeeded: game={actualPath}, hook={loadedHookPath}");
                    return true;
                }
                catch (Exception ex)
                {
                    LogService.Error($"Runtime validation process check failed PID={process.Id}: {ex.Message}");
                }
                finally
                {
                    process.Dispose();
                }
            }

            warning =
                "游戏进程路径和启动器写入配置的游戏目录不一致。\n\n" +
                $"启动器期望启动：\n{expectedGamePath}\n\n" +
                "实际检测到：\n" +
                (actualPaths.Count > 0 ? string.Join("\n", actualPaths.Distinct()) : "<无法读取实际路径>") +
                "\n\n请确认启动器选择的是你正在运行的那份游戏目录。";
            LogService.Error($"Runtime validation failed: game path mismatch. Expected={expectedGamePath}, Actual={string.Join(" | ", actualPaths)}");
            return false;
        }
        catch (Exception ex)
        {
            warning = "游戏联机组件运行时校验失败：" + ex.Message;
            LogService.Error($"Runtime validation failed: {ex}");
            return false;
        }
    }





    private static bool LooksLikeOriginalLauncherProcess(Process p)
    {
        string processName = SafeProcessName(p);

        // 避免误杀浏览器里打开的 GitHub 页面。
        if (IsKnownBrowserProcess(processName))
            return false;

        string text = GetProcessIdentityText(p);
        if (string.IsNullOrWhiteSpace(text))
            return false;

        if (text.Contains("github.com/unixoide/5th-echelon", StringComparison.OrdinalIgnoreCase))
            return true;

        if (text.Contains("unixoide/5th-echelon", StringComparison.OrdinalIgnoreCase))
            return true;

        if (text.Contains("5th-echelon", StringComparison.OrdinalIgnoreCase)
            && !text.Contains("SplinterCellCNLauncher", StringComparison.OrdinalIgnoreCase))
            return true;

        if (text.Contains("5th Echelon", StringComparison.OrdinalIgnoreCase)
            && !text.Contains("SplinterCellCNLauncher", StringComparison.OrdinalIgnoreCase))
            return true;

        return false;
    }

    private static bool IsKnownBrowserProcess(string processName)
    {
        return processName.Equals("chrome", StringComparison.OrdinalIgnoreCase)
            || processName.Equals("msedge", StringComparison.OrdinalIgnoreCase)
            || processName.Equals("firefox", StringComparison.OrdinalIgnoreCase)
            || processName.Equals("brave", StringComparison.OrdinalIgnoreCase)
            || processName.Equals("opera", StringComparison.OrdinalIgnoreCase)
            || processName.Equals("vivaldi", StringComparison.OrdinalIgnoreCase);
    }

    private static string SafeProcessName(Process p)
    {
        try
        {
            return p.ProcessName ?? "";
        }
        catch
        {
            return "";
        }
    }

    private static string GetProcessIdentityText(Process p)
    {
        var parts = new List<string>();

        try { parts.Add(p.ProcessName); } catch { }
        try { parts.Add(p.MainWindowTitle); } catch { }

        try
        {
            string? path = p.MainModule?.FileName;
            if (!string.IsNullOrWhiteSpace(path))
            {
                parts.Add(path);
                parts.Add(Path.GetFileNameWithoutExtension(path));

                try
                {
                    var info = FileVersionInfo.GetVersionInfo(path);
                    parts.Add(info.FileDescription ?? "");
                    parts.Add(info.ProductName ?? "");
                    parts.Add(info.OriginalFilename ?? "");
                    parts.Add(info.InternalName ?? "");
                    parts.Add(info.Comments ?? "");
                }
                catch
                {
                }
            }
        }
        catch
        {
        }

        return string.Join(" | ", parts.Where(x => !string.IsNullOrWhiteSpace(x)));
    }

    private async Task RunLaunchFlowAsync()
    {
        SetBusy(true, "准备启动...");


        try
        {
            SetBusy(true, "清理冲突...");


            string username = txtUsername.Text.Trim();
            string password = txtPassword.Password;
            string? passwordScope = _autoPasswordScope;
            string server = txtServer.Text.Trim();
            bool hostMode = IsHostMode();

            if (string.IsNullOrWhiteSpace(username))
                throw new Exception(L("请输入账号。", "Please enter a username."));

            if (!IsValidUsernameInput(username))
                throw new Exception(L("账号只能使用英文、数字、下划线、短横线或点，不能包含中文或空格。", "Username can only contain letters, numbers, underscores, hyphens, or dots. Chinese characters and spaces are not allowed."));

            if (string.IsNullOrEmpty(password))
                throw new Exception(L("请输入密码。", "Please enter a password."));

            if (!IsValidPasswordInput(password))
                throw new Exception(L("密码含无法传递给游戏的字符，或超出输入容量。", "Password contains an unsupported encoding or exceeds the input capacity."));

            if (!PasswordPolicy.IsValidForNativeGame(password))
                throw new Exception(L("游戏最多支持 63 个 UTF-8 字节的密码（中文等字符可能占多个字节）。当前密码过长，已取消启动；账号和已保存的密码未被修改。", "The game supports passwords up to 63 UTF-8 bytes; some characters use multiple bytes. This password is too long, so launch was cancelled. Your account and saved password have not been changed."));

            // 服务器地址为空或格式不正确时，交给启动前网络检测给出对应提示，
            // 不再直接进入游戏。
            SetBusy(true, "检测网络...");
            await RefreshServerStatusAsync(keepButtonDisabled: true, showFailureDialog: true);
            if (_serverStatusKind != ServerStatusKind.Normal || !_lastServerReachable)
            {
                LogService.Info("Launch cancelled because server status is not normal.");
                return;
            }

            server = txtServer.Text.Trim();
            string gameDir = ResolveGameDirectoryOrAsk();
            _firewallService.EnsureFirewallRulesBestEffort(GetLauncherBaseDirectory(), gameDir);
            string gameExe = GetSelectedGameExecutable();
            string gameLabel = GetSelectedGameLabel();


            LogService.Info($"Launch version: {gameLabel} ({gameExe})");
            LogService.Info($"Game directory: {gameDir}");

            SetBusy(true, "检测网络...");
            var local = GetLanLocalStatus();
            if (!local.IsOk || local.Bind == null) throw new IOException(string.IsNullOrEmpty(local.Error) ? "未找到可用本机 IPv4，请检查网卡或手动选择。" : local.Error);

            var bind = local.Bind;

            LogService.Info($"Bind IP: {bind.IpAddress}");

            if (!await ConfirmNetworkRiskBeforeLaunchAsync())
            {
                LogService.Info("Launch cancelled by user because network risk warning was shown.");
                return;
            }

            if (hostMode)
            {
                SetBusy(true, L("启动服务端...", "Starting server..."));
                server = await EnsureHostServerReadyAsync(bind.IpAddress, username);
                SetServerAddressProgrammatically(server, null, resetStatus: false);
                LogService.Info($"Host mode server address set to {server}");
            }

            SetBusy(true, "校验游戏...");
            _gameLocatorService.ValidateGameDirectory(gameDir, gameExe);


            SetBusy(true, "连接服务器...");
            // The host endpoint can change during network readiness. Never reuse another
            // endpoint's auto-filled credential captured before that asynchronous work.
            if (!CanUseCapturedPassword(passwordScope, server, username))
                throw new IOException(L("服务器地址已变化，请确认账号和密码后重新启动。", "The server address changed. Confirm your credentials and start again."));
            var login = await _authService.LoginAsync(server, username, password, _settings.BindOverride);

            if (login.Status == LoginStatus.UserNotFound)
            {
                var create = await ShowConfirmDialogAsync(
                    title: L("账号不存在", "Account Not Found"),
                    message: L($"服务器上不存在账号 [{username}]。\n是否使用当前账号和密码注册新账号？", $"Account [{username}] does not exist on the server.\nRegister a new account with the current username and password?"),
                    yesText: L("注册账号", "Register"),
                    noText: L("取消", "Cancel"));

                if (create != MessageBoxResult.Yes)
                {
                    LogService.Info("用户取消注册。");
                    return;
                }

                SetBusy(true, "注册账号...");
                var register = await _authService.RegisterAsync(server, username, password, username, _settings.BindOverride);
                if (register.Status != RegisterStatus.Success)
                    throw new Exception(register.Message);

                LogService.Info("Account registered successfully.");
                await ShowRecoveryCodeDialogAsync(register.RecoveryCode);

                SetBusy(true, "连接服务器...");
                login = await _authService.LoginAsync(server, username, password, _settings.BindOverride);
            }

            if (login.Status != LoginStatus.Success)
                throw new Exception(login.Message);

            LogService.Info("Server authentication succeeded.");
            await RememberSuccessfulLoginAsync(server,username,password);

            SetBusy(true, "写入配置...");
            string accountId=login.AccountId;
            if(string.IsNullOrWhiteSpace(accountId)) throw new IOException("服务端未返回账号标识，取消启动。");

            string gameplayMode=GetSelectedGameplayMode();
            var request = new GameLaunchRequest(gameDir, gameExe, gameplayMode, RuntimePaths.Root,
                username, password, accountId, server, _settings.BindOverride, GetSelectedFovScale());
            Process process = await new GameLaunchPreparation(_gameLaunchService).StartAsync(
                request,
                async () =>
                {
                    await PrepareFusionFixForLaunchAsync(gameDir, gameplayMode);
                    // Saves remain game-owned; no launcher reads or writes.
                    SaveSettingsFromUi();
                },
                stage =>
                {
                    switch (stage)
                    {
                        case LaunchPreparationStage.Deploying:
                            SetBusy(true, "部署组件...");
                            break;
                        case LaunchPreparationStage.PreparingConfiguration:
                            SetBusy(true, "写入配置...");
                            break;
                        case LaunchPreparationStage.Starting:
                            SetBusy(true, L("启动游戏...", "Starting game..."));
                            LogService.Info($"Starting game: {gameExe}");
                            _launchStartedUtc=DateTime.UtcNow;
                            SetGameStartingState(true);
                            _isBusy=true;btnLaunch.IsEnabled=false;
                            break;
                    }
                });
            // Close can drain the session before this successful continuation resumes.
            // Release only our returned handle; closing is not permission to kill.
            if (_closingBackgroundTasks)
            {
                process.Dispose();
                return;
            }
            _isBusy=false;

            PauseMusicForGameSession();
            _runningGameProcessName = Path.GetFileNameWithoutExtension(gameExe);
            SetGameStartingState(true);

            // Do not rely on the returned Process object. Some launch paths return a short-lived stub process,
            // especially when Uplay login or network initialization is slow. Wait for the real game process first.
            MonitorGameStartAndLifetime(gameDir, gameExe, gameLabel);

            try
            {
                process.Dispose();
            }
            catch
            {
                // Ignore process handle cleanup failure.
            }

            LogService.Info("Game launch command sent. Waiting for actual game process.");
            WindowState = WindowState.Minimized;
        }
        catch (Exception ex)
        {
            CancelGameMonitor();
            await _gameLaunchService.StopAsync();
            if (_closingBackgroundTasks) return;
            var unconfirmed = _gameLaunchService.UnconfirmedLaunch;
            if (unconfirmed is { HasExited: false })
            {
                _unconfirmedLaunchSession = unconfirmed.SessionId;
                _runningGameProcessName = Path.GetFileNameWithoutExtension(unconfirmed.ExecutablePath);
                SetGameRunningState(true);
                MonitorUnconfirmedLaunch(unconfirmed.SessionId);
                AppendUiError(ex.Message);
                await ShowInfoDialogAsync(L("启动未确认", "Launch Not Confirmed"),
                    L("游戏尚未确认启动，但进程仍在运行。请点击“结束游戏”后重试。", "Game startup was not confirmed, but its process is still running. Click End Game, then retry.") + "\n\n" + ex.Message);
            }
            else
            {
                if (unconfirmed != null) _gameLaunchService.ReleaseExitedUnconfirmedLaunch(unconfirmed.SessionId);
                _unconfirmedLaunchSession = null;
                _isGameStarting = false;
                AppendUiError(ex.Message);
                string message = ex.Message;
                if (_gameLaunchService.IsCreatedProcessFailure(ex))
                    message += "\n\n" + L("如另有游戏进程仍在运行，请手动关闭后重试。", "If another game process is still running, close it manually before retrying.");
                await ShowInfoDialogAsync("启动失败", message);
            }
        }
        finally
        {

            if (!_closingBackgroundTasks) SetBusy(false);
        }
    }

    private void MonitorUnconfirmedLaunch(Guid expectedSession)
    {
        if (_closingBackgroundTasks) return;
        _gameMonitor.Start(async token =>
        {
            try
            {
                bool released = await _gameLaunchService.WaitForUnconfirmedLaunchExitAsync(expectedSession, token);
                if (!released || token.IsCancellationRequested) return;
                await Dispatcher.InvokeAsync(() =>
                {
                    if (token.IsCancellationRequested || _isEndingGame || _unconfirmedLaunchSession != expectedSession) return;
                    _unconfirmedLaunchSession = null;
                    SetGameRunningState(false);
                    BringLauncherToFront("Unconfirmed launch process exited");
                });
            }
            catch (OperationCanceledException) when (token.IsCancellationRequested) { }
            catch (Exception ex)
            {
                LogService.Error("Unconfirmed launch exit observation: " + ex.GetType().Name);
            }
        });
    }

    private void MonitorGameStartAndLifetime(string gameDir, string gameExe, string gameLabel)
    {
        if (_closingBackgroundTasks) return;
        string processName = Path.GetFileNameWithoutExtension(gameExe);
        string expectedGamePath = Path.GetFullPath(Path.Combine(gameDir, gameExe));
        string expectedHookPath = Path.GetFullPath(Path.Combine(gameDir, "uplay_r1_loader.dll"));

        CancelGameMonitor();
        var launchSession = _gameLaunchService.SessionId;

        _gameMonitor.Start(async token =>
        {
            try
            {
                LogService.Info($"Waiting for actual game process without timeout: {processName}. User may click restart if needed.");

                await WaitForProcessStableAppearAsync(
                    processName,
                    TimeSpan.FromSeconds(GameStableAppearSeconds),
                    token, launchSession);
                if (token.IsCancellationRequested || _isEndingGame)
                    return;

                string? runtimeWarning = null;
                bool runtimeOk = false;

                Dispatcher.Invoke(() =>
                {
                    if (token.IsCancellationRequested) return;
                    _runningGameProcessName = processName;
                    LogService.Info($"Actual {gameLabel} game process detected and stable: {processName}");
                    runtimeOk = ValidateGameRuntimeAfterStart(processName, expectedGamePath, expectedHookPath, out runtimeWarning);
                    _gameLaunchService.MarkDiagnosticStable();
                    SetGameRunningState(true);
                });

                if (!runtimeOk && !string.IsNullOrWhiteSpace(runtimeWarning))
                {
                    _ = Dispatcher.InvokeAsync(async () =>
                    {
                        if (token.IsCancellationRequested) return;
                        WindowState = WindowState.Normal;
                        Activate();
                        await ShowInfoDialogAsync("联机组件未确认生效", runtimeWarning);
                    });
                }

                await WaitForProcessReallyExitAsync(processName, token);
                if (token.IsCancellationRequested || _isEndingGame)
                    return;

                Dispatcher.Invoke(() =>
                {
                    if (token.IsCancellationRequested) return;
                    LogService.Info($"{gameLabel} game process ended.");
                    SetGameRunningState(false);
                    BringLauncherToFront($"{gameLabel} game exited");
                });
            }
            catch (GameEarlyExitException)
            {
                await Dispatcher.InvokeAsync(async () => {
                    if (token.IsCancellationRequested) return;
                    SetGameRunningState(false); SetGameStartingState(false); BringLauncherToFront("Confirmed early game exit");
                    await ShowInfoDialogAsync(L("游戏提前退出", "Game exited during startup"), L("已记录本次游戏的异常退出。请导出诊断包；如果在设置的高级诊断中使用了诊断启动，可在那里单独导出异常现场。", "This game's abnormal exit was recorded. Export diagnostics. If you used Diagnostic launch under Settings > Advanced diagnostics, export the private exception capture there separately."));
                });
            }
            catch (TaskCanceledException)
            {
                // Normal when the user ends the game or closes the launcher.
            }
            catch (Exception ex)
            {
                LogService.Error($"Failed while monitoring actual game process: {ex}");
            }
            finally
            {
                if(launchSession.HasValue) await _gameLaunchService.StopAsync(launchSession);
            }
        });
    }

    private sealed class GameEarlyExitException : Exception { }

    private async Task WaitForProcessStableAppearAsync(
        string processName,
        TimeSpan stableDuration,
        CancellationToken token, Guid? diagnosticSession = null)
    {
        Stopwatch? stableSw = null;
        bool loggedFirstSeen = false;

        while (true)
        {
            token.ThrowIfCancellationRequested();
            if(diagnosticSession.HasValue && _gameLaunchService.HasConfirmedEarlyExit(diagnosticSession)) throw new GameEarlyExitException();

            bool running = IsProcessNameRunning(processName);

            if (running)
            {
                stableSw ??= Stopwatch.StartNew();

                if (!loggedFirstSeen)
                {
                    LogService.Info($"Game process appeared. Waiting for stable presence: {processName}, stable={stableDuration.TotalSeconds:0}s");
                    loggedFirstSeen = true;
                }

                if (stableSw.Elapsed >= stableDuration)
                    return;
            }
            else
            {
                if (stableSw != null)
                {
                    LogService.Info($"Game process disappeared before stable threshold. Continue waiting: {processName}");
                }

                stableSw = null;
                loggedFirstSeen = false;
            }

            await Task.Delay(GameProcessProbeIntervalMs, token);
        }
    }

    private async Task WaitForProcessReallyExitAsync(string processName, CancellationToken token)
    {
        int missingCount = 0;

        while (true)
        {
            token.ThrowIfCancellationRequested();

            bool running = IsProcessNameRunning(processName);
            if (running)
            {
                if (missingCount > 0)
                {
                    LogService.Info($"Game process appeared again before exit confirmation. Continue monitoring: {processName}");
                }

                missingCount = 0;
            }
            else
            {
                missingCount++;

                if (missingCount >= GameExitMissingChecks)
                {
                    LogService.Info($"Game process missing for {missingCount} consecutive checks. Treat as exited: {processName}");
                    break;
                }
            }

            await Task.Delay(GameProcessProbeIntervalMs, token);
        }
    }

    private void CancelGameMonitor() => _gameMonitor.Cancel();

    private bool IsProcessNameRunning(string processName)
    {
        foreach(var p in Process.GetProcessesByName(processName))
        { using(p) {try {if(IsOwnedGameProcess(p)) return true;} catch{} } }
        return false;
    }

    private string GetLauncherBaseDirectory()
    {
        return AppContext.BaseDirectory;
    }

    private string GetServerDirectory() => Path.Combine(LogService.AppDataDir,"server",_hostBindIp.Replace('.','_'));

    private string GetServerExePath() => Path.Combine(RuntimePaths.Root,"SCBL.LAN.Server.exe");

    private async Task<string> EnsureHostServerReadyAsync(string localIp,string hostName)
    {
        if(!IPAddress.TryParse(localIp,out var ip) || !ScblLan.LanConnection.IsUnicast(ip)) throw new IOException("请选择本机 IPv4。");
        using(var probe=new Socket(AddressFamily.InterNetwork,SocketType.Dgram,ProtocolType.Udp)) probe.Bind(new IPEndPoint(ip,0));
        if(IsDedicatedServerRunning() && _hostBindIp!=localIp) throw new IOException("请先退出房主模式再更换地址。");
        StopHostBroadcast();
        _hostBindIp=localIp;
        await _hostRuntime.InitializeAsync(GetServerExePath(),GetServerDirectory(),localIp);
        _hostRuntime.Start(GetServerExePath(),GetServerDirectory());
        string endpoint = $"{localIp}:{_hostRuntime.ApiPort}";
        var ready=await _hostRuntime.WaitReadyAsync(endpoint,localIp,TimeSpan.FromSeconds(25));
        if(!ready) {StopDedicatedServer("readiness failed");throw new IOException("服务端未就绪。检查端口占用、防火墙及本轮服务端日志；不会终止外部服务端。");}
        // Isolated UI regression starts the real host but must not advertise a synthetic room to players.
        if (!App.HostCheckMode) StartHostBroadcast(hostName,localIp);
        if (!string.IsNullOrEmpty(_hostRuntime.PortNotice)) LogService.Info(_hostRuntime.PortNotice);
        return endpoint;
    }





    private bool IsDedicatedServerRunning() => _hostRuntime.IsRunning;

    private bool StopDedicatedServer(string reason)
    {
        try
        {
            StopHostBroadcast();
            _hostRuntime.Stop();
            LogService.Info("本启动器服务端停止："+reason);
            return true;
        }
        catch (Exception ex)
        {
            LogService.Error("本启动器服务端停止失败："+reason+" "+ex);
            return false;
        }
    }



    private void StartHostBroadcast(string hostName, string hostIp)
    {
        StopHostBroadcast();
        if (_closingBackgroundTasks || !IsHostMode() || App.HostCheckMode)
            return;

        _hostBroadcastIntent = new(hostName, hostIp, _settings.BindOverride, _hostRuntime.ApiPort);
        string sessionId = Guid.NewGuid().ToString("N");
        _hostSessionId = sessionId;
        _currentHostBroadcastName = hostName;
        _currentHostBroadcastIp = hostIp;
        _hostBroadcast.Start(async token =>
        {
            try
            {
                using var udp = CreateDiscoveryUdpClient(localIp: hostIp, localPort: 0);
                var targets = GetHostBroadcastTargets(hostIp);

                LogService.Info($"Host broadcast socket bound to {hostIp}, targets: {string.Join(", ", targets.Select(t => t.ToString()))}");

                while (!token.IsCancellationRequested)
                {
                    try
                    {
                        if (!_isHostModeCached || !IsDedicatedServerRunning())
                            break;

                        var local = GetLanLocalStatus();
                        if (!local.IsOk)
                        {
                            // The UI monitor can skip while busy. Preserve this loss, but never pause a newer generation.
                            await Dispatcher.InvokeAsync(() =>
                            {
                                if (_hostSessionId == sessionId) PauseHostBroadcast();
                            });
                            break;
                        }

                        string message = BuildHostBroadcastMessage(hostName, hostIp, sessionId, _hostRuntime.ApiPort);
                        byte[] bytes = Encoding.UTF8.GetBytes(message);

                        foreach (var target in targets)
                        {
                            token.ThrowIfCancellationRequested();
                            try
                            {
                                await udp.SendAsync(bytes.AsMemory(), target, token);
                            }
                            catch (Exception ex)
                            {
                                LogService.Error($"Host broadcast send failed to {target}: {ex.Message}");
                            }
                        }

                        await Task.Delay(HostBroadcastIntervalMs, token);
                    }
                    catch (TaskCanceledException)
                    {
                        break;
                    }
                    catch (Exception ex)
                    {
                        LogService.Error($"Host broadcast failed: {ex}");
                        try
                        {
                            await Task.Delay(HostBroadcastIntervalMs, token);
                        }
                        catch
                        {
                            break;
                        }
                    }
                }
            }
            catch (Exception ex)
            {
                LogService.Error($"Host broadcast socket init failed: {ex}");
            }
            finally
            {
                // A failed or completed worker must not leave its publication looking active.
                // The UI owns these fields; a late predecessor may never pause a newer session.
                try
                {
                    if (!Dispatcher.HasShutdownStarted && !Dispatcher.HasShutdownFinished)
                    {
                        await Dispatcher.InvokeAsync(() =>
                        {
                            if (_hostSessionId == sessionId) PauseHostBroadcast();
                        });
                    }
                }
                catch (OperationCanceledException) when (_closingBackgroundTasks || Dispatcher.HasShutdownStarted || Dispatcher.HasShutdownFinished) { }
                catch (InvalidOperationException) when (Dispatcher.HasShutdownStarted || Dispatcher.HasShutdownFinished) { }
            }
        });
        LogService.Info($"Host broadcast started: {hostName}, {hostIp}");
    }

    private static UdpClient CreateDiscoveryUdpClient(string? localIp, int localPort)
    {
        var udp = new UdpClient(AddressFamily.InterNetwork);
        try
        {
        udp.EnableBroadcast = true;
        udp.Client.SetSocketOption(SocketOptionLevel.Socket, SocketOptionName.ReuseAddress, true);

        IPAddress bindAddress = IPAddress.Any;
        if (!string.IsNullOrWhiteSpace(localIp) && IPAddress.TryParse(localIp, out var parsed))
            bindAddress = parsed;

        udp.Client.Bind(new IPEndPoint(bindAddress, localPort));
        return udp;
        }
        catch { udp.Dispose(); throw; }
    }

    private static IReadOnlyList<IPEndPoint> GetHostBroadcastTargets(string? localIp)
    {
        return HostDiscoveryTargets.GetTargets(localIp, HostBroadcastPort);
    }

    private static string BuildHostBroadcastMessage(string hostName, string hostIp, string? sessionId, int apiPort = 50051)
    {
        return string.Join("\n", new[]
        {
            "SCBL_LAN_HOST",
            "Version=1",
            $"HostName={hostName}",
            $"HostIP={hostIp}",
            $"ApiPort={apiPort}",
            $"SessionId={sessionId ?? Guid.NewGuid().ToString("N")}",
            "State=ServerReady",
            $"StartedAt={DateTimeOffset.UtcNow.ToUnixTimeSeconds()}"
        });
    }

    private static string BuildHostDiscoveryRequestMessage(string clientIp)
    {
        return string.Join("\n", new[]
        {
            "SCBL_LAN_HOST_QUERY",
            "Version=1",
            $"ClientIP={clientIp}",
            $"RequestedAt={DateTimeOffset.UtcNow.ToUnixTimeSeconds()}"
        });
    }

    private static bool IsHostDiscoveryRequestMessage(string text)
    {
        string firstLine = text.Replace("\r\n", "\n").Replace('\r', '\n')
            .Split('\n', StringSplitOptions.RemoveEmptyEntries)
            .FirstOrDefault()?.Trim() ?? "";

        return firstLine.Equals("SCBL_LAN_HOST_QUERY", StringComparison.OrdinalIgnoreCase);
    }

    private void StopHostBroadcast()
    {
        _hostBroadcastIntent = null;
        PauseHostBroadcast();
    }

    private void PauseHostBroadcast()
    {
        // Retain the requested publication across a temporary network loss, not its public session identity.
        _hostBroadcast.Cancel();
        _hostSessionId = null;
        _currentHostBroadcastName = null;
        _currentHostBroadcastIp = null;
    }

    private void TryResumeHostBroadcast(HostBroadcastIntent? intent, string checkedServer, string checkedBind)
    {
        if (intent == null || !ReferenceEquals(intent, _hostBroadcastIntent) || _hostSessionId != null
            || _closingBackgroundTasks || !IsHostMode() || App.HostCheckMode || !IsDedicatedServerRunning()
            || _hostBindIp != intent.Ip || _hostRuntime.ApiPort != intent.ApiPort
            || checkedBind != intent.BindOverride || _settings.BindOverride != checkedBind)
            return;

        var endpoint = ScblLan.ServerAddress.Parse(checkedServer);
        if (!string.Equals(endpoint.Host, intent.Ip, StringComparison.OrdinalIgnoreCase) || endpoint.Port != intent.ApiPort)
            return;
        var local = GetLanLocalStatus();
        if (!local.IsOk || local.Bind!.IpAddress != intent.Ip)
            return;

        // The successful check belongs to this retained intent. Start drains the old worker and creates one fresh session.
        StartHostBroadcast(intent.Name, intent.Ip);
    }

    private void StartHostDiscoveryListener()
    {
        if (_closingBackgroundTasks) return;
        _hostDiscovery.Start(async token =>
        {
            try
            {
                using var udp = CreateDiscoveryUdpClient(localIp: null, localPort: HostBroadcastPort);
                udp.Client.ReceiveTimeout = 1000;

                LogService.Info($"Host discovery listener started on UDP {HostBroadcastPort}. Outgoing discovery packets bind to Lan IP when available.");
                DateTime lastQueryUtc = DateTime.MinValue;

                while (!token.IsCancellationRequested)
                {
                    try
                    {
                        var remote = new IPEndPoint(IPAddress.Any, 0);
                        byte[] buffer = udp.Receive(ref remote);
                        if (token.IsCancellationRequested) break;
                        if(buffer.Length>4096) continue;
                        if (buffer.Length == LanPresenceRegistry.MaxBytes) { ReplyToPresence(buffer, remote); continue; }
                        string text = Encoding.UTF8.GetString(buffer);

                        if (IsHostDiscoveryRequestMessage(text))
                        {
                            await ReplyToHostDiscoveryRequestAsync(udp, remote, token);
                            continue;
                        }

                        var host = ParseHostBroadcastMessage(text);
                        if (host == null)
                            continue;

                        if (!IPAddress.TryParse(host.HostIp,out var announced) || !ScblLan.LanConnection.IsUnicast(announced) || !announced.Equals(remote.Address))
                            continue;

                        await Dispatcher.InvokeAsync(() => { if (!token.IsCancellationRequested) HandleDiscoveredHost(host); });
                    }
                    catch (SocketException ex) when (ex.SocketErrorCode == SocketError.TimedOut)
                    {
                        if ((DateTime.UtcNow - lastQueryUtc).TotalMilliseconds >= HostBroadcastIntervalMs)
                        {
                            lastQueryUtc = DateTime.UtcNow;
                            await SendHostDiscoveryQueryAsync(udp, token);
                        }
                    }
                }
            }
            catch (OperationCanceledException)
            {
            }
            catch (SocketException ex)
            {
                LogService.Error($"Host discovery socket failed: {ex.Message}");
            }
            catch (Exception ex)
            {
                LogService.Error($"Host discovery failed: {ex}");
            }
        });
    }

    private async Task SendHostDiscoveryQueryAsync(UdpClient udp, CancellationToken token)
    {
        // 非房主启动游戏后，不再主动发送“寻找房主”包。
        // 监听可以继续保留，但游戏启动中/运行中不再主动打扰网络。
        if (token.IsCancellationRequested || _isHostModeCached || _isGameStarting || _isGameRunning)
            return;

        var local = GetLanLocalStatus();
        if (!local.IsOk || local.Bind == null)
            return;

        string bindIp = local.Bind.IpAddress;
        string message = BuildHostDiscoveryRequestMessage(bindIp);
        byte[] bytes = Encoding.UTF8.GetBytes(message);

        // Lan 专用版：主动寻找房主包也强制从本机 Lan IP 发出，
        // 避免系统默认路由把 UDP 包丢到 Wi-Fi / 有线网卡 / 其他虚拟网卡。
        using var boundUdp = CreateDiscoveryUdpClient(localIp: bindIp, localPort: 0);

        foreach (var target in GetHostBroadcastTargets(bindIp))
        {
            try
            {
                await boundUdp.SendAsync(bytes, bytes.Length, target);
            }
            catch (Exception ex)
            {
                LogService.Error($"Host discovery query send failed from {bindIp} to {target}: {ex.Message}");
            }
        }
    }

    private async Task ReplyToHostDiscoveryRequestAsync(UdpClient udp, IPEndPoint remote, CancellationToken token)
    {
        if (token.IsCancellationRequested)
            return;

        if (!_isHostModeCached || !IsDedicatedServerRunning())
            return;

        string? hostName = _currentHostBroadcastName;
        string? hostIp = _currentHostBroadcastIp;
        string? sessionId = _hostSessionId;

        if (string.IsNullOrWhiteSpace(hostName) || string.IsNullOrWhiteSpace(hostIp) || string.IsNullOrWhiteSpace(sessionId))
            return;

        var local = GetLanLocalStatus();
        if (!local.IsOk || local.Bind == null)
            return;

        if (remote.Address.Equals(IPAddress.Parse(local.Bind.IpAddress)))
            return;

        string message = BuildHostBroadcastMessage(hostName, hostIp, sessionId, _hostRuntime.ApiPort);
        byte[] bytes = Encoding.UTF8.GetBytes(message);

        try
        {
            // Lan 专用版：房主单播回复也强制从本机 Lan IP 发出。
            using var boundUdp = CreateDiscoveryUdpClient(localIp: local.Bind.IpAddress, localPort: 0);
            await boundUdp.SendAsync(bytes, bytes.Length, remote);
            if (remote.Port != HostBroadcastPort)
                await boundUdp.SendAsync(bytes, bytes.Length, new IPEndPoint(remote.Address, HostBroadcastPort));
        }
        catch (Exception ex)
        {
            LogService.Error($"Host discovery reply failed from {local.Bind.IpAddress} to {remote}: {ex.Message}");
        }
    }

    private static DiscoveredHost? ParseHostBroadcastMessage(string text)
    {
        string[] lines = text.Replace("\r\n", "\n").Replace('\r', '\n').Split('\n', StringSplitOptions.RemoveEmptyEntries);
        if (lines.Length == 0 || !lines[0].Trim().Equals("SCBL_LAN_HOST", StringComparison.OrdinalIgnoreCase))
            return null;

        var dict = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        foreach (string raw in lines.Skip(1))
        {
            int index = raw.IndexOf('=');
            if (index <= 0)
                continue;

            dict[raw[..index].Trim()] = raw[(index + 1)..].Trim();
        }

        dict.TryGetValue("HostIP", out string? hostIp);
        if (string.IsNullOrWhiteSpace(hostIp))
            return null;

        dict.TryGetValue("HostName", out string? hostName);
        dict.TryGetValue("SessionId", out string? sessionId);
        dict.TryGetValue("ApiPort", out string? portText);
        int.TryParse(portText, out int port);
        if (port <= 0 || port > 65535) return null;
        if(!dict.TryGetValue("Version",out var version) || version!="1" || !dict.TryGetValue("State",out var state) || state!="ServerReady") return null;
        if(hostIp.Length>64 || (hostName?.Length??0)>64 || (sessionId?.Length??0)>64) return null;

        return new DiscoveredHost
        {
            HostIp = hostIp.Trim(),
            HostName = string.IsNullOrWhiteSpace(hostName) ? "未知房主" : hostName.Trim(),
            SessionId = string.IsNullOrWhiteSpace(sessionId) ? hostIp.Trim() : sessionId.Trim(),
            ApiPort = port,
            LastSeenUtc = DateTime.UtcNow
        };
    }

    private bool CanSwitchDiscoveredHost()
    {
        return !_closingBackgroundTasks && !_isBusy && !IsHostMode()
            && !_isGameStarting && !_isGameRunning && toolsOverlay.Visibility != Visibility.Visible;
    }

    private void HandleDiscoveredHost(DiscoveredHost host)
    {
        if (IsHostMode())
            return;

        RemoveExpiredDiscoveredHosts();
        if(_discoveredHosts.Count>=64 && !_discoveredHosts.ContainsKey(host.SessionId)) return;
        _discoveredHosts[host.SessionId] = host;

        var activeHosts = GetActiveDiscoveredHosts();
        if (activeHosts.Count == 0)
            return;

        // Retain discovery while an operation owns the current endpoint; prompt on a later idle beacon.
        if (!CanSwitchDiscoveredHost())
        {
            LogService.Info("Host discovery retained without prompting while endpoint switching is unavailable.");
            return;
        }

        if (activeHosts.Count > 1)
        {
            UpdateDetectedHostLabelForMultipleHosts(activeHosts);

            string currentHostInList = NormalizeServerHost(txtServer.Text.Trim());
            if (activeHosts.Any(h => h.HostIp.Equals(currentHostInList, StringComparison.OrdinalIgnoreCase))
                && (_serverStatusKind != ServerStatusKind.Normal || !_lastServerReachable))
            {
                QueueAutomaticServerCheckAfterLanOnline();
            }

            if (!_multiHostDialogShownThisSession && _dialogTcs == null && !_isHostSelectionDialogOpen && toolsOverlay.Visibility != Visibility.Visible)
            {
                _multiHostDialogShownThisSession = true;
                _ = ShowHostSelectionAndApplyAsync(
                    activeHosts,
                    title: "发现多个房主",
                    message: "当前发现多个房主，请选择要连接的房主。");
            }

            return;
        }

        DiscoveredHost only = activeHosts[0];
        string current = txtServer.Text.Trim();

        if (IsServerAddressEmptyOrDefault(current))
        {
            string promptKey = "AUTO|" + only.SessionId + "|" + only.Endpoint;
            if (_lastSingleHostAutoFillPromptKey == promptKey || _dialogTcs != null || _isHostSelectionDialogOpen)
                return;

            _lastSingleHostAutoFillPromptKey = promptKey;
            _ = PromptAutoFillSingleHostAsync(only);
            return;
        }

        string currentHost = NormalizeServerHost(current);
        ScblLan.ServerAddress currentAddress;
        try { currentAddress = ScblLan.ServerAddress.Parse(current); }
        catch (ArgumentException) { return; } // A beacon must not interrupt a partially edited address.
        if (currentHost.Equals(only.HostIp, StringComparison.OrdinalIgnoreCase) &&
            currentAddress.Port == only.ApiPort)
        {
            txtDetectedHostName.Text = BuildDetectedHostLabel(only, canSwitch: false);
            UpdateServerPlaceholderVisibility();

            if (_serverStatusKind != ServerStatusKind.Normal || !_lastServerReachable)
                QueueAutomaticServerCheckAfterLanOnline();

            return;
        }
        string switchPromptKey = "SWITCH|" + only.SessionId + "|" + only.Endpoint;
        if (_lastHostSwitchPromptKey == switchPromptKey || _dialogTcs != null || _isHostSelectionDialogOpen)
            return;

        _lastHostSwitchPromptKey = switchPromptKey;
        _ = PromptSwitchSingleHostAsync(only, current);
    }

    private List<DiscoveredHost> GetActiveDiscoveredHosts()
    {
        RemoveExpiredDiscoveredHosts();

        return _discoveredHosts.Values
            .GroupBy(h => h.HostIp, StringComparer.OrdinalIgnoreCase)
            .Select(group => group.OrderByDescending(h => h.LastSeenUtc).First())
            .OrderByDescending(h => h.LastSeenUtc)
            .ToList();
    }

    private async Task PromptAutoFillSingleHostAsync(DiscoveredHost host)
    {
        if (!CanSwitchDiscoveredHost())
            return;

        var result = await ShowConfirmDialogAsync(
            title: L("发现房主", "Host Found"),
            message: L(
                $"发现 {GetHostNameForLabel(host)} 房主，是否自动填写 IP 信息？\n\n房主：{GetHostNameForLabel(host)}\n地址：{host.HostIp}",
                $"Host {GetHostNameForLabel(host)} found. Auto-fill the IP address?\n\nHost: {GetHostNameForLabel(host)}\nAddress: {host.HostIp}"),
            yesText: L("自动填写", "Auto Fill"),
            noText: L("取消", "Cancel"));

        if (!CanSwitchDiscoveredHost())
        {
            if (_lastSingleHostAutoFillPromptKey == "AUTO|" + host.SessionId + "|" + host.Endpoint)
                _lastSingleHostAutoFillPromptKey = null;
            return;
        }
        if (result == MessageBoxResult.Yes && IsServerAddressEmptyOrDefault(txtServer.Text.Trim()))
        {
            await ApplyDiscoveredHostAsync(host, $"已填写房主：{host.DisplayName}");
        }
    }

    private async Task PromptSwitchSingleHostAsync(DiscoveredHost host, string current)
    {
        if (!CanSwitchDiscoveredHost())
            return;

        var result = await ShowConfirmDialogAsync(
            title: L("发现新房主", "New Host Found"),
            message: L(
                $"发现房主：{host.DisplayName}\n\n当前服务器地址已经填写为：{current}\n\n是否切换到这个房主？",
                $"Host found: {host.DisplayName}\n\nThe current server address is: {current}\n\nSwitch to this host?"),
            yesText: L("切换", "Switch"),
            noText: L("忽略", "Ignore"));

        if (!CanSwitchDiscoveredHost())
        {
            if (_lastHostSwitchPromptKey == "SWITCH|" + host.SessionId + "|" + host.Endpoint)
                _lastHostSwitchPromptKey = null;
            return;
        }
        if (result == MessageBoxResult.Yes)
            await ApplyDiscoveredHostAsync(host, $"已切换到房主：{host.DisplayName}");
    }

    private async Task ShowHostSelectionAndApplyAsync(IReadOnlyList<DiscoveredHost> hosts, string title, string message)
    {
        if (hosts.Count == 0 || !CanSwitchDiscoveredHost())
            return;

        var selected = ShowHostSelectionDialog(hosts, title, message);
        if (!CanSwitchDiscoveredHost())
        {
            _multiHostDialogShownThisSession = false;
            return;
        }
        if (selected == null) return;

        await ApplyDiscoveredHostAsync(selected, $"已选择房主：{selected.DisplayName}");
    }

    private async Task ApplyDiscoveredHostAsync(DiscoveredHost host, string statusText)
    {
        if (!CanSwitchDiscoveredHost()) return;
        SetServerAddressProgrammatically(host.Endpoint, host.HostName);
        RememberNonHostServerAddressIfUseful(host.Endpoint);
        txtDetectedHostName.Text = BuildDetectedHostLabel(host, canSwitch: GetActiveDiscoveredHosts().Count > 1);
        UpdateServerPlaceholderVisibility();
        await RefreshServerStatusAsync(showFailureDialog: false);
    }

    private void UpdateDetectedHostLabelForMultipleHosts(IReadOnlyList<DiscoveredHost> activeHosts)
    {
        string currentHost = NormalizeServerHost(txtServer.Text.Trim());
        var matched = activeHosts.FirstOrDefault(h => h.HostIp.Equals(currentHost, StringComparison.OrdinalIgnoreCase));

        txtDetectedHostName.Text = matched != null
            ? BuildDetectedHostLabel(matched, canSwitch: true)
            : "房主：多个可选";

        UpdateServerPlaceholderVisibility();
    }

    private static string GetHostNameForLabel(DiscoveredHost host)
    {
        return string.IsNullOrWhiteSpace(host.HostName) ? "未知房主" : host.HostName.Trim();
    }

    private static string BuildDetectedHostLabel(DiscoveredHost host, bool canSwitch)
    {
        // 右侧胶囊按钮固定显示“切换 ▾”，这里不再额外追加箭头，避免界面重复。
        return $"房主：{GetHostNameForLabel(host)}";
    }

    private DiscoveredHost? ShowHostSelectionDialog(IReadOnlyList<DiscoveredHost> hosts, string title, string message)
    {
        if (_dialogTcs != null || _isHostSelectionDialogOpen || hosts.Count == 0)
            return null;

        _isHostSelectionDialogOpen = true;
        DiscoveredHost? selectedHost = hosts[0];

        try
        {
            var dialog = new Window
            {
                Title = title,
                Owner = this,
                Opacity = App.PreviewMode ? 0 : 1,
                ShowActivated = !App.PreviewMode,
                MaxWidth = SystemParameters.WorkArea.Width,
                MaxHeight = SystemParameters.WorkArea.Height,
                WindowStartupLocation = WindowStartupLocation.CenterOwner,
                SizeToContent = SizeToContent.WidthAndHeight,
                ResizeMode = ResizeMode.NoResize,
                ShowInTaskbar = false,
                Background = Brushes.Transparent,
                Foreground = new SolidColorBrush((Color)ColorConverter.ConvertFromString("#EAF2FF")),
                FontFamily = FontFamily,
                WindowStyle = WindowStyle.None,
                AllowsTransparency = true
            };

            var root = new System.Windows.Controls.Border
            {
                Width = 520,
                Background = new SolidColorBrush((Color)ColorConverter.ConvertFromString("#F20B0F11")),
                BorderBrush = new SolidColorBrush((Color)ColorConverter.ConvertFromString("#55FFFFFF")),
                BorderThickness = new Thickness(1),
                CornerRadius = new CornerRadius(0),
                Padding = new Thickness(28),
                Effect = new System.Windows.Media.Effects.DropShadowEffect
                {
                    Color = Colors.Black,
                    BlurRadius = 28,
                    ShadowDepth = 8,
                    Opacity = 0.35
                }
            };

            var panel = new System.Windows.Controls.StackPanel();
            root.Child = panel;

            panel.Children.Add(new System.Windows.Controls.TextBlock
            {
                Text = LocalizeDialogTitleText(title),
                Foreground = new SolidColorBrush((Color)ColorConverter.ConvertFromString("#F5FAFF")),
                FontSize = 22,
                FontWeight = FontWeights.Bold,
                Margin = new Thickness(0, 0, 0, 10)
            });

            panel.Children.Add(new System.Windows.Controls.TextBlock
            {
                Text = hosts.Count > 1
                    ? L("当前发现多个房主，请选择要连接的房主。", "Multiple hosts were found. Choose the host to connect to.")
                    : L("当前发现 1 个房主，请选择是否连接。", "One host was found. Choose whether to connect."),
                Foreground = new SolidColorBrush((Color)ColorConverter.ConvertFromString("#B8C7D9")),
                FontSize = 13,
                LineHeight = 22,
                TextWrapping = TextWrapping.Wrap,
                Margin = new Thickness(0, 0, 0, 18)
            });

            var listFrame = new System.Windows.Controls.Border
            {
                Background = new SolidColorBrush((Color)ColorConverter.ConvertFromString("#090D0F")),
                BorderBrush = new SolidColorBrush((Color)ColorConverter.ConvertFromString("#35FFFFFF")),
                BorderThickness = new Thickness(1),
                CornerRadius = new CornerRadius(0),
                Padding = new Thickness(10),
                Margin = new Thickness(0, 0, 0, 22),
                Height = Math.Min(252, Math.Max(198, hosts.Count * 50 + 18))
            };

            var scrollViewer = new System.Windows.Controls.ScrollViewer
            {
                VerticalScrollBarVisibility = System.Windows.Controls.ScrollBarVisibility.Auto,
                HorizontalScrollBarVisibility = System.Windows.Controls.ScrollBarVisibility.Disabled
            };

            var hostRows = new System.Windows.Controls.StackPanel();
            scrollViewer.Content = hostRows;
            listFrame.Child = scrollViewer;
            panel.Children.Add(listFrame);

            dialog.PreviewKeyDown += (_,e)=>{if(e.Key==Key.Escape){dialog.Close();e.Handled=true;}};
            var rowButtons = new List<System.Windows.Controls.Button>();

            System.Windows.Controls.Button CreateHostRowButton(DiscoveredHost host)
            {
                var grid = new System.Windows.Controls.Grid();
                grid.ColumnDefinitions.Add(new System.Windows.Controls.ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
                grid.ColumnDefinitions.Add(new System.Windows.Controls.ColumnDefinition { Width = GridLength.Auto });

                var nameBlock = new System.Windows.Controls.TextBlock
                {
                    Text = GetHostNameForLabel(host),
                    Foreground = new SolidColorBrush((Color)ColorConverter.ConvertFromString("#F5FAFF")),
                    FontSize = 14,
                    FontWeight = FontWeights.Bold,
                    VerticalAlignment = VerticalAlignment.Center,
                    TextTrimming = TextTrimming.CharacterEllipsis,
                    Margin = new Thickness(2, 0, 16, 0)
                };
                System.Windows.Controls.Grid.SetColumn(nameBlock, 0);
                grid.Children.Add(nameBlock);

                var ipBlock = new System.Windows.Controls.TextBlock
                {
                    Text = host.Endpoint,
                    Foreground = new SolidColorBrush((Color)ColorConverter.ConvertFromString("#A5ADB2")),
                    FontSize = 13,
                    FontWeight = FontWeights.SemiBold,
                    VerticalAlignment = VerticalAlignment.Center
                };
                System.Windows.Controls.Grid.SetColumn(ipBlock, 1);
                grid.Children.Add(ipBlock);

                var button = new System.Windows.Controls.Button
                {
                    Content = grid,
                    Height = 42,
                    Margin = new Thickness(0, 0, 0, 8),
                    Padding = new Thickness(14, 0, 14, 0),
                    Background = new SolidColorBrush((Color)ColorConverter.ConvertFromString("#101416")),
                    BorderBrush = new SolidColorBrush((Color)ColorConverter.ConvertFromString("#35FFFFFF")),
                    BorderThickness = new Thickness(1),
                    Cursor = Cursors.Hand,
                    HorizontalContentAlignment = HorizontalAlignment.Stretch,
                    Template = CreateHostRowTemplate()
                };

                button.Click += (_, _) =>
                {
                    selectedHost = host;
                    UpdateHostRowSelection(rowButtons, button);
                };

                button.MouseDoubleClick += (_, _) =>
                {
                    selectedHost = host;
                    dialog.DialogResult = true;
                };

                return button;
            }

            foreach (var host in hosts)
            {
                var button = CreateHostRowButton(host);
                rowButtons.Add(button);
                hostRows.Children.Add(button);
            }

            if (rowButtons.Count > 0)
                UpdateHostRowSelection(rowButtons, rowButtons[0]);

            var buttons = new System.Windows.Controls.StackPanel
            {
                Orientation = System.Windows.Controls.Orientation.Horizontal,
                HorizontalAlignment = HorizontalAlignment.Right
            };

            var cancelButton = new System.Windows.Controls.Button
            {
                Content = L("取消", "Cancel"),
                Style = TryFindResource("CinematicLinkButton") as Style,
                Margin = new Thickness(0, 0, 12, 0)
            };

            var chooseButton = new System.Windows.Controls.Button
            {
                Content = L("连接房主", "Connect Host"),
                Style = TryFindResource("CinematicDialogAction") as Style
            };

            cancelButton.Click += (_, _) =>
            {
                selectedHost = null;
                dialog.Close();
            };

            chooseButton.Click += (_, _) => dialog.DialogResult = true;

            buttons.Children.Add(cancelButton);
            buttons.Children.Add(chooseButton);
            panel.Children.Add(buttons);

            dialog.Content = root;
            return dialog.ShowDialog() == true ? selectedHost : null;
        }
        finally
        {
            _isHostSelectionDialogOpen = false;
        }
    }

    private static ControlTemplate CreateHostRowTemplate()
    {
        var borderFactory = new FrameworkElementFactory(typeof(System.Windows.Controls.Border));
        borderFactory.SetValue(System.Windows.Controls.Border.BackgroundProperty, new TemplateBindingExtension(System.Windows.Controls.Button.BackgroundProperty));
        borderFactory.SetValue(System.Windows.Controls.Border.BorderBrushProperty, new TemplateBindingExtension(System.Windows.Controls.Button.BorderBrushProperty));
        borderFactory.SetValue(System.Windows.Controls.Border.BorderThicknessProperty, new TemplateBindingExtension(System.Windows.Controls.Button.BorderThicknessProperty));
        borderFactory.SetValue(System.Windows.Controls.Border.CornerRadiusProperty, new CornerRadius(0));

        var presenterFactory = new FrameworkElementFactory(typeof(System.Windows.Controls.ContentPresenter));
        presenterFactory.SetValue(System.Windows.Controls.ContentPresenter.HorizontalAlignmentProperty, HorizontalAlignment.Stretch);
        presenterFactory.SetValue(System.Windows.Controls.ContentPresenter.VerticalAlignmentProperty, VerticalAlignment.Center);
        presenterFactory.SetValue(System.Windows.Controls.ContentPresenter.MarginProperty, new Thickness(0));
        borderFactory.AppendChild(presenterFactory);

        return new ControlTemplate(typeof(System.Windows.Controls.Button))
        {
            VisualTree = borderFactory
        };
    }

    private static void UpdateHostRowSelection(IReadOnlyList<System.Windows.Controls.Button> buttons, System.Windows.Controls.Button selectedButton)
    {
        foreach (var button in buttons)
        {
            bool selected = ReferenceEquals(button, selectedButton);
            button.Background = new SolidColorBrush((Color)ColorConverter.ConvertFromString(selected ? "#203B2B" : "#101416"));
            button.BorderBrush = new SolidColorBrush((Color)ColorConverter.ConvertFromString(selected ? "#8FD2A0" : "#35FFFFFF"));
        }
    }


    private void DetectedHostName_Click(object sender, RoutedEventArgs e)
    {
        if (_closingBackgroundTasks || _isBusy || toolsOverlay.Visibility == Visibility.Visible) return;
        if (IsHostMode())
            return;

        if (_isGameStarting || _isGameRunning)
        {
            _ = ShowInfoDialogAsync(
                L("游戏运行中", "Game Is Running"),
                L("游戏启动中或运行中不切换房主。\n\n请退出游戏后再重新选择房主。", "Host switching is disabled while the game is starting or running.\n\nExit the game before selecting another host."));
            return;
        }

        var activeHosts = GetActiveDiscoveredHosts();
        if (activeHosts.Count == 0)
        {
            _ = ShowInfoDialogAsync(
                L("暂无可选房主", "No Hosts Available"),
                L("当前没有发现有效房主。\n\n请确认房主已经开启房主模式，并且双方网络能够互访。 ", "No valid host has been discovered.\n\nPlease confirm the host has enabled Host Mode and both sides are online in LAN network."));
            return;
        }

        _ = ShowHostSelectionAndApplyAsync(
            activeHosts,
            title: activeHosts.Count > 1 ? L("选择房主", "Select Host") : L("当前发现的房主", "Available Host"),
            message: activeHosts.Count > 1 ? L("请选择要连接的房主。", "Choose the host to connect to.") : L("当前只发现一个房主，可以直接连接。", "Only one host is currently available. You can connect directly."));
    }

    private void RemoveExpiredDiscoveredHosts()
    {
        DateTime cutoff = DateTime.UtcNow.AddSeconds(-HostDiscoveryExpireSeconds);
        foreach (string key in _discoveredHosts.Where(pair => pair.Value.LastSeenUtc < cutoff).Select(pair => pair.Key).ToList())
            _discoveredHosts.Remove(key);
    }

    private static bool IsServerAddressEmptyOrDefault(string server)
    {
        if (string.IsNullOrWhiteSpace(server))
            return true;

        string host = NormalizeServerHost(server);
        return host.Equals("127.0.0.1", StringComparison.OrdinalIgnoreCase)
            || host.Equals("localhost", StringComparison.OrdinalIgnoreCase);
    }

    private static string NormalizeServerHost(string server)
    {
        string text = server.Trim();
        if (text.StartsWith("http://", StringComparison.OrdinalIgnoreCase) ||
            text.StartsWith("https://", StringComparison.OrdinalIgnoreCase))
        {
            if (Uri.TryCreate(text, UriKind.Absolute, out var uri))
                return uri.Host;
        }

        int slash = text.IndexOf('/');
        if (slash >= 0)
            text = text[..slash];

        int colon = text.LastIndexOf(':');
        if (colon > 0 && text.Count(c => c == ':') == 1)
            text = text[..colon];

        return text.Trim();
    }

    private void TitleBar_MouseLeftButtonDown(object sender, MouseButtonEventArgs e)
    {
        if (e.ButtonState != MouseButtonState.Pressed)
            return;

        try
        {
            DragMove();
        }
        catch
        {
            // Ignore drag failure while a modal state is changing.
        }
    }

    private void WindowMinimizeButton_Click(object sender, RoutedEventArgs e)
    {
        WindowState = WindowState.Minimized;
    }

    private void WindowCloseButton_Click(object sender, RoutedEventArgs e)
    {
        Close();
    }


    private async Task ShowRecoveryCodeDialogAsync(string recoveryCode)
    {
        // A successfully registered account's one-time code must not be lost
        // when an unrelated information/confirmation overlay is already open.
        while (_dialogTcs is { } pendingDialog)
            await pendingDialog.Task;
        await ShowInfoDialogAsync(
            L("账号已创建", "Account Created"),
            L("请保存下面的一次性恢复码。可选中码框后按 Ctrl+A、Ctrl+C 复制，并保存在安全位置。以后在“忘记密码”中使用；使用后会生成新码。此码不会自动写入日志或剪贴板，关闭后清除。",
              "Save the one-time recovery code below. Select the code box and press Ctrl+A, Ctrl+C to copy it to a safe place. Use Forgot password to recover your account; using it creates a new code. It is not automatically logged or copied, and is cleared when this dialog closes."),
            recoveryCode);
    }

    private void ShowDialogOverlay()
    {
        _dialogPreviousFocus = Keyboard.FocusedElement;
        dialogOverlay.Visibility = Visibility.Visible;
        dialogOverlay.UpdateLayout();
        FocusDialogDefault();
    }

    private void FocusDialogDefault()
    {
        if (btnDialogNo.Visibility == Visibility.Visible && btnDialogNo.IsEnabled)
            btnDialogNo.Focus();
        else
            btnDialogYes.Focus();
    }

    private void DialogBoundary_PreviewGotKeyboardFocus(object sender, KeyboardFocusChangedEventArgs e)
    {
        if (dialogOverlay.Visibility != Visibility.Visible)
            return;
        if (e.NewFocus is DependencyObject target &&
            (ReferenceEquals(target, dialogOverlay) || dialogOverlay.IsAncestorOf(target)))
            return;
        e.Handled = true;
        FocusDialogDefault();
    }

    private async Task<MessageBoxResult> ShowTimedConfirmDialogAsync(string title, string message, string yesText, string noText, int seconds)
    {
        if (_dialogTcs != null)
            return MessageBoxResult.None;

        _dialogTcs = new TaskCompletionSource<MessageBoxResult>();
        var localTcs = _dialogTcs;

        txtDialogTitle.Text = LocalizeDialogTitleText(title);
        txtDialogMessage.Text = message;
        btnDialogNo.Content = LocalizeDialogButtonText(noText);
        btnDialogNo.Visibility = Visibility.Visible;
        btnDialogYes.IsEnabled = false;
        ShowDialogOverlay();

        for (int left = seconds; left > 0; left--)
        {
            if (!ReferenceEquals(_dialogTcs, localTcs) || localTcs.Task.IsCompleted)
                break;

            btnDialogYes.Content = $"{LocalizeDialogButtonText(yesText)}（{left}）";

            try
            {
                await Task.Delay(1000);
            }
            catch
            {
                break;
            }
        }

        if (ReferenceEquals(_dialogTcs, localTcs) && !localTcs.Task.IsCompleted)
        {
            btnDialogYes.Content = LocalizeDialogButtonText(yesText);
            btnDialogYes.IsEnabled = true;
        }

        return await localTcs.Task;
    }

    private Task<MessageBoxResult> ShowConfirmDialogAsync(string title, string message, string yesText = "确定", string noText = "取消")
    {
        if (_dialogTcs != null)
            return Task.FromResult(MessageBoxResult.None);

        _dialogTcs = new TaskCompletionSource<MessageBoxResult>();

        txtDialogTitle.Text = LocalizeDialogTitleText(title);
        txtDialogMessage.Text = message;
        btnDialogYes.Content = LocalizeDialogButtonText(yesText);
        btnDialogYes.IsEnabled = true;
        btnDialogNo.Content = LocalizeDialogButtonText(noText);
        btnDialogNo.Visibility = Visibility.Visible;
        ShowDialogOverlay();

        return _dialogTcs.Task;
    }

    private async Task ShowInfoDialogAsync(string title, string message, string? selectableValue = null)
    {
        if (_dialogTcs != null)
            return;

        _dialogTcs = new TaskCompletionSource<MessageBoxResult>();

        txtDialogTitle.Text = LocalizeDialogTitleText(title);
        txtDialogMessage.Text = message;
        txtDialogRecoveryCode.Text = selectableValue ?? string.Empty;
        txtDialogRecoveryCode.Visibility = selectableValue == null ? Visibility.Collapsed : Visibility.Visible;
        System.Windows.Automation.AutomationProperties.SetName(txtDialogRecoveryCode, L("一次性恢复码", "One-time recovery code"));
        btnDialogYes.Content = LocalizeDialogButtonText("知道了");
        btnDialogYes.IsEnabled = true;
        btnDialogNo.Visibility = Visibility.Collapsed;
        ShowDialogOverlay();

        await _dialogTcs.Task;
    }

    private void DialogYesButton_Click(object sender, RoutedEventArgs e)
    {
        CompleteDialog(MessageBoxResult.Yes);
    }

    private void DialogOverlay_PreviewKeyDown(object sender, System.Windows.Input.KeyEventArgs e)
    {
        if(e.Key==System.Windows.Input.Key.Escape && dialogOverlay.Visibility==Visibility.Visible)
        {CompleteDialog(MessageBoxResult.No);e.Handled=true;}
    }

    private void DialogNoButton_Click(object sender, RoutedEventArgs e)
    {
        CompleteDialog(MessageBoxResult.No);
    }

    private void CompleteDialog(MessageBoxResult result)
    {
        var tcs = _dialogTcs;
        _dialogTcs = null;

        dialogOverlay.Visibility = Visibility.Collapsed;
        txtDialogRecoveryCode.Clear();
        txtDialogRecoveryCode.Visibility = Visibility.Collapsed;
        var previousFocus = _dialogPreviousFocus;
        _dialogPreviousFocus = null;
        if (previousFocus is UIElement previous && previous.IsVisible && previous.IsEnabled && previous.Focusable)
            Keyboard.Focus(previous);
        btnDialogYes.IsEnabled = true;
        btnDialogNo.Visibility = Visibility.Visible;

        tcs?.TrySetResult(result);
    }
    private bool IsOwnedGameProcess(Process process)
    {
        // Enumeration can still return a terminated process while handles exist.
        // Path/start time establish ownership, not whether it is still running.
        if(process.HasExited) return false;
        string? path=process.MainModule?.FileName;
        return path!=null && Path.GetDirectoryName(path)!.Equals(Path.GetFullPath(_gameDir),StringComparison.OrdinalIgnoreCase)
            && process.StartTime.ToUniversalTime()>=_launchStartedUtc.AddSeconds(-1) && !process.HasExited;
    }
    private void PopulateAdapters()
    {
        List<BindIpCandidate> adapters = new();
        bool refreshed = false;
        try { adapters = _networkService.GetBindIpCandidates(); refreshed = true; }
        catch (Exception ex) when (ex is NetworkInformationException or UnauthorizedAccessException or System.Security.SecurityException)
        {
            LogService.Error("Adapter refresh failed: " + ex.GetType().Name);
            cmbBindAdapter.ToolTip = L("无法刷新网卡列表，请稍后重新打开重试。", "Could not refresh adapters. Reopen this list to retry.");
            // Preserve a previous snapshot; on initial failure still offer the automatic entry.
            if (cmbBindAdapter.Items.Count > 0) return;
        }

        bool wasRestoring = _restoringInteractiveSetting;
        _restoringInteractiveSetting = true;
        try
        {
            cmbBindAdapter.Items.Clear();
            cmbBindAdapter.Items.Add(new ComboBoxItem {Content=L("自动按服务器路由（开房多网卡时须选择）", "Automatic route (choose an adapter for hosting)"),Tag=""});
            foreach (var a in adapters)
                cmbBindAdapter.Items.Add(new ComboBoxItem {Content=$"{a.IpAddress}  ·  {a.AdapterName}",Tag=a.IpAddress});
            // A vanished manual address is not an implicit switch to automatic routing.
            cmbBindAdapter.SelectedItem = cmbBindAdapter.Items.Cast<ComboBoxItem>()
                .FirstOrDefault(item => (string)item.Tag == _settings.BindOverride);
            if (refreshed) cmbBindAdapter.ToolTip = null;
        }
        finally { _restoringInteractiveSetting = wasRestoring; }
    }

    private void BindAdapter_DropDownOpened(object? sender, EventArgs e)
    {
        if (!IsLoaded || App.PreviewMode || _restoringInteractiveSetting || _closingBackgroundTasks
            || _isBusy || _isGameRunning || _isGameStarting || IsDedicatedServerRunning()
            || _statusRefreshRunning || _networkCheckRunning || !cmbBindAdapter.IsEnabled) return;
        PopulateAdapters();
    }
    private void BindAdapter_SelectionChanged(object sender,SelectionChangedEventArgs e)
    {
        if(!IsLoaded || App.PreviewMode || _restoringInteractiveSetting || cmbBindAdapter.SelectedItem is not ComboBoxItem item) return;
        if(_isBusy || _isGameRunning || _isGameStarting || IsDedicatedServerRunning()) return;
        string previous = _settings.BindOverride;
        object? previousSelection = e.RemovedItems.Count > 0 ? e.RemovedItems[0] : null;
        if (!TryPersistInteractiveSetting(() => _settings.BindOverride = (string)item.Tag,
            () => { _settings.BindOverride = previous; cmbBindAdapter.SelectedItem = previousSelection; })) return;
        _lastServerReachable=false;_serverAddressEditedAfterLastCheck=true;
        _=RefreshServerStatusAsync();
    }

}

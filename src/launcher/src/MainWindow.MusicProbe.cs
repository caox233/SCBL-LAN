using System.Text;
using System.Windows;
using System.Windows.Media;
using SplinterCellCNLauncher.Models;
using SplinterCellCNLauncher.Services;
namespace SplinterCellCNLauncher;
public partial class MainWindow
{
    internal async Task<string> RunMusicCheckAsync()
    {
        if (!App.PreviewMode) throw new InvalidOperationException("Isolated preview required");
        var report = new StringBuilder(); int failed = 0;
        void Check(bool ok, string name) { report.AppendLine((ok ? "PASS " : "FAIL ") + name); if (!ok) failed++; }
        _musicProbeMuted = true;
        _settings = new LauncherSettings();
        Check(_settings.MusicEnabled, "new profile defaults on");
        Check(EmbeddedResourceService.EmbeddedFileExists("launcher_bgm.mp3"), "track embedded");
        string path = ResolveLauncherMusicPath() ?? throw new IOException("Missing music");
        var opened = new TaskCompletionSource<bool>();
        void Opened(object? s, EventArgs e) => opened.TrySetResult(true);
        void Failed(object? s, ExceptionEventArgs e) => opened.TrySetException(e.ErrorException);
        _musicPlayer.MediaOpened += Opened; _musicPlayer.MediaFailed += Failed;
        try
        {
            PlayStartupMusicIfEnabled();
            await opened.Task.WaitAsync(TimeSpan.FromSeconds(15));
            Check(_musicPlayer.HasAudio && _musicPlayer.NaturalDuration.HasTimeSpan && _musicPlayer.NaturalDuration.TimeSpan.TotalSeconds > 1, "actual embedded MP3 decoded");
            await Task.Delay(350);
            Check(_musicPlayer.Volume == 0 && _musicPlayer.Position > TimeSpan.Zero, "muted decoder position advances");
            report.AppendLine("DURATION_SECONDS " + _musicPlayer.NaturalDuration.TimeSpan.TotalSeconds.ToString(System.Globalization.CultureInfo.InvariantCulture));
            MusicToggleButton_Click(this, new RoutedEventArgs());
            Check(!_settingsService.Load().MusicEnabled && !_musicIsPlaying, "disable persisted and stopped");
            MusicToggleButton_Click(this, new RoutedEventArgs());
            Check(_settingsService.Load().MusicEnabled && _musicIsPlaying, "enable persisted and replay requested");
            PauseMusicForGameSession();
            Check(_launcherMediaFrozenForGameSession && !_musicIsPlaying, "game session stops decoder");
            MusicToggleButton_Click(this, new RoutedEventArgs());
            MusicToggleButton_Click(this, new RoutedEventArgs());
            PlayStartupMusicIfEnabled(forceReplay: true);
            Check(!_musicIsPlaying, "toggle cannot restart during game session");
            _isGameRunning = false;
            PlayStartupMusicIfEnabled(forceReplay: true);
            Check(!_musicIsPlaying, "game exit does not restart music");
            UpdateMusicButton();
            Check(btnMusicToggle.Content?.ToString() == "\uE767" && btnMusicToggle.Visibility == Visibility.Visible && spTitleButtons.Children.Contains(btnMusicToggle), "top right music control visible");
        }
        finally
        {
            StopMusic(); _musicPlayer.MediaOpened -= Opened; _musicPlayer.MediaFailed -= Failed;
        }
        await Task.Delay(150);
        using (var released = new FileStream(path, FileMode.Open, FileAccess.ReadWrite, FileShare.None)) Check(released.Length > 0, "decoder releases track");
        report.AppendLine("FAILED " + failed);
        return report.ToString();
    }
}

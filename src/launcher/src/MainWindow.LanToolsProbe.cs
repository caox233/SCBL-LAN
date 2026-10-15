using System.Text.Json;
using System.Windows;
using ScblLan;
using SplinterCellCNLauncher.Services;
namespace SplinterCellCNLauncher;
public partial class MainWindow
{
 internal string RunLanToolsChecks(string report)
 {
  if(!App.PreviewMode)throw new InvalidOperationException("Preview fixture required");
  int passed=0,failed=0;var lines=new List<string>();
  void Check(bool b,string label){lines.Add((b?"PASS ":"FAIL ")+label);if(b)passed++;else failed++;}
  SettingsButton_Click(this,new());Check(toolsOverlay.Visibility==Visibility.Visible&&toolsHomePanel.Visibility==Visibility.Visible,"settings button opens themed centralized panel");
  AdvancedDiagnosticsButton_Click(this,new());Check(advancedDiagnosticsPanel.Visibility==Visibility.Visible&&toolsHomePanel.Visibility==Visibility.Collapsed&&btnCaptureOnce.Visibility==Visibility.Visible&&btnExportCapture.Visibility==Visibility.Visible,"advanced diagnostics remains reachable inside settings");
  Check(!_gameLaunchService.CaptureNextLaunch&&(txtAdvancedDiagnosticsInfo.Text.Contains("敏感")||txtAdvancedDiagnosticsInfo.Text.Contains("sensitive",StringComparison.OrdinalIgnoreCase)),"opening advanced diagnostics does not enable capture and warns of sensitive dumps");
  txtServer.Text="192.0.2.10:50051";txtUsername.Text="fixture-user";
  ForgotPasswordButton_Click(this,new());Check(recoveryPanel.Visibility==Visibility.Visible&&_recoveryServer=="192.0.2.10:50051"&&_recoveryUser=="fixture-user","forgot password captures endpoint and identity");
  recoveryCode.Password="SYNTHETIC-CODE";recoveryPassword.Password="x";recoveryConfirm.Password="x";
  RestoreSavesButton_Click(this,new());Check(savesPanel.Visibility==Visibility.Visible&&recoveryCode.Password==""&&recoveryPassword.Password=="","changing page clears recovery secrets");
  Check(txtSavePaths.Text=="Official: "+_saveGameService.SaveDirectory+"\nMOD: "+Path.Combine(_saveGameService.SaveDirectory,"MOD"),"save page shows exact Official and nested Saves/MOD paths without reading them");
  PlayersButton_Click(this,new());Check(playersPanel.Visibility==Visibility.Visible,"players entry opens quality view");
  ShowToolsPreview("players");Check(playersList.Items.Count==2&&txtQualityDetails.Text.Contains("8.2"),"presence rows and measured-quality presentation bind correctly");
  rootGrid.Measure(new Size(Width,Height));rootGrid.Arrange(new Rect(0,0,Width,Height));rootGrid.UpdateLayout();
  IEnumerable<System.Windows.Controls.TextBlock> Texts(DependencyObject d)
  {
   if(d is System.Windows.Controls.TextBlock t)yield return t;
   for(int i=0;i<System.Windows.Media.VisualTreeHelper.GetChildrenCount(d);i++)foreach(var child in Texts(System.Windows.Media.VisualTreeHelper.GetChild(d,i)))yield return child;
  }
  var rows=Texts(playersList).Where(t=>t.Text.StartsWith("Preview")).ToArray();
  Check(rows.Length==2&&rows.All(t=>t.Foreground is System.Windows.Media.SolidColorBrush b&&b.Color.R>128&&b.Color.G>128),"player rows have readable light foreground on dark settings panel");
  SetToolsOperation(true);ToolsClose_Click(this,new());Check(toolsOverlay.Visibility==Visibility.Visible&&!btnToolsClose.IsEnabled&&!btnAdvancedDiagnosticsPage.IsEnabled&&!btnCaptureOnce.IsEnabled&&!btnExportCapture.IsEnabled,"active transaction cannot close settings or start private capture");SetToolsOperation(false);
  txtReplacementCode.Text="SYNTHETIC-REPLACEMENT";ToolsClose_Click(this,new());Check(toolsOverlay.Visibility==Visibility.Collapsed&&txtReplacementCode.Text=="","closing settings clears replacement recovery code");
  _settings.Language="en-US";ApplyLocalization();Check((string)btnSettings.Content=="Settings"&&(string)btnSavesPage.Content=="Restore saves"&&(string)btnAdvancedDiagnosticsPage.Content=="Advanced diagnostics"&&(string)btnCaptureOnce.Content=="Diagnostic launch"&&(string)btnExportCapture.Content=="Export private capture","new controls localize to English");
  // Exercise packaged embedded templates on disposable paths, never the player's save directory.
  string root=Path.Combine(Path.GetDirectoryName(Path.GetFullPath(report))!,"packaged-saves-"+Guid.NewGuid().ToString("N"));
  var service=new SaveGameService(Path.Combine(root,"Saves"),Path.Combine(root,"backups"),()=>false);
  Check(service.DirectoryFor("MOD")==Path.Combine(root,"Saves","MOD"),"packaged MOD destination independently matches native Saves/MOD layout");
  using var stream=typeof(SaveGameService).Assembly.GetManifestResourceStream("SCBL.LAN.DefaultSaves.manifest.json");
  Check(stream!=null,"packaged save manifest present");
  using var doc=JsonDocument.Parse(stream!);
  foreach(string mode in new[]{"Official","MOD"})
  {
   string backup=service.RestoreBundled(mode,RuntimePaths.Root);int count=0;
   foreach(var row in doc.RootElement.GetProperty("Files").EnumerateObject().Where(x=>x.Name.StartsWith(mode+"/")))
   {string destination=mode=="MOD"?Path.Combine(root,"Saves","MOD"):Path.Combine(root,"Saves");Check(GameRuntime.Hash(Path.Combine(destination,row.Name[(mode.Length+1)..])).Equals(row.Value.GetProperty("sha256").GetString(),StringComparison.OrdinalIgnoreCase),"packaged "+row.Name+" restores exact authorized bytes at native path");count++;}
   Check(Directory.Exists(backup)&&File.ReadAllText(Path.Combine(backup,"manifest.json")).Contains("Complete")&&count==(mode=="MOD"?5:4),"packaged "+mode+" restore transaction completes");
  }
  Check(!Directory.Exists(Path.Combine(root,"MOD")),"packaged restore never creates wrong sibling MOD directory");
  return string.Join("\n",lines)+$"\nRESULT {passed} passed; {failed} failed; isolated UI and save fixtures only\n";
 }
}

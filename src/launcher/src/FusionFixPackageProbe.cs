using System.IO;
using System.Security.Cryptography;
using SplinterCellCNLauncher.Services;

namespace SplinterCellCNLauncher;

// Explicit CLI self-test: only creates fresh synthetic sibling directories to the supplied report.
internal static class FusionFixPackageProbe
{
    internal static async Task RunAsync(string reportPath)
    {
        string report=Path.GetFullPath(reportPath);
        string root=Path.Combine(Path.GetDirectoryName(report)!, "fusionfix-probe-"+Guid.NewGuid().ToString("N"));
        string system=Path.Combine(root,"MOD","SYSTEM");Directory.CreateDirectory(system);
        var output=new List<string>();
        void Check(bool value,string name){if(!value)throw new IOException("FAIL "+name);output.Add("PASS "+name);}
        // The explicit fixture must not append to the launcher's real data/log store.
        var service=new FusionFixDeploymentService(
            () => typeof(FusionFixDeploymentService).Assembly.GetManifestResourceStream("SCBL.LAN.FusionFix.zip")
                ?? throw new InvalidDataException("Embedded offline FusionFix asset is missing."),
            FusionFixDeploymentService.AssetSha256, reportResult: _ => { });
        var first=await service.EnsureForLaunchAsync(system,"MOD");
        Check(first.Outcome==FusionFixDeploymentOutcome.Installed,"packaged embedded FusionFix installs for MOD");
        Check(File.Exists(Path.Combine(system,"version.dll")) && File.Exists(Path.Combine(system,"scripts/SplinterCellBlacklist.FusionFix.asi")),"loader and ASI installed into exact SYSTEM");
        string ini=Path.Combine(system,"scripts/SplinterCellBlacklist.FusionFix.ini");
        Check(File.Exists(ini)&&ScblLan.GameRuntime.Hash(ini).Equals("7afff9f1ca632991fb4cbf965e4810e3480935436e386bb782c41f93c4c32252",StringComparison.OrdinalIgnoreCase),"missing INI receives exact embedded default");
        string loader=Path.Combine(system,"version.dll");string hash=Convert.ToHexString(SHA256.HashData(File.ReadAllBytes(loader)));
        var second=await service.EnsureForLaunchAsync(system,"MOD");
        Check(second.Outcome==FusionFixDeploymentOutcome.AlreadyPresent && Convert.ToHexString(SHA256.HashData(File.ReadAllBytes(loader)))==hash,"repeated packaged deployment preserves identical files");
        File.WriteAllText(ini,"synthetic custom INI");
        using(var locked=new FileStream(ini,FileMode.Open,FileAccess.ReadWrite,FileShare.None)){
            var kept=await service.EnsureForLaunchAsync(system,"MOD");
            using var reader=new StreamReader(locked,leaveOpen:true);
            Check(kept.Outcome==FusionFixDeploymentOutcome.AlreadyPresent&&reader.ReadToEnd()=="synthetic custom INI","existing exclusively locked INI remains untouched");
        }
        File.Delete(ini);var time=new DateTime(2020,1,1,0,0,0,DateTimeKind.Utc);File.SetLastWriteTimeUtc(loader,time);
        var completed=await service.EnsureForLaunchAsync(system,"MOD");
        Check(completed.InstalledFileCount==1&&completed.ReplacedFileCount==0&&File.Exists(ini),"only missing INI completed when binaries already match");
        Check(File.GetLastWriteTimeUtc(loader)==time&&Convert.ToHexString(SHA256.HashData(File.ReadAllBytes(loader)))==hash,"INI completion does not rewrite matching loader");
        File.WriteAllText(loader,"synthetic older FusionFix loader");
        string oldHash=ScblLan.GameRuntime.Hash(loader);
        var replaced=await service.EnsureForLaunchAsync(system,"MOD");
        Check(replaced.ReplacedFileCount==1 && Convert.ToHexString(SHA256.HashData(File.ReadAllBytes(loader)))==hash,"different packaged FusionFix loader automatically replaced");
        Check(replaced.BackupDirectory!=null && Directory.EnumerateFiles(replaced.BackupDirectory,"version.dll",SearchOption.AllDirectories).Any(p=>ScblLan.GameRuntime.Hash(p)==oldHash),"exact old FusionFix loader retained in backup");
        string official=Path.Combine(root,"Official");Directory.CreateDirectory(official);
        var installed=await service.EnsureForLaunchAsync(official,"Official");
        string officialLoader=Path.Combine(official,"SYSTEM/version.dll"),officialAsi=Path.Combine(official,"SYSTEM/scripts/SplinterCellBlacklist.FusionFix.asi"),officialIni=Path.Combine(official,"SYSTEM/scripts/SplinterCellBlacklist.FusionFix.ini");
        Check(installed.Outcome==FusionFixDeploymentOutcome.Installed && installed.InstalledFileCount==first.InstalledFileCount,"packaged embedded FusionFix also installs for Official");
        Check(ScblLan.GameRuntime.Hash(officialLoader)==ScblLan.GameRuntime.Hash(loader) && ScblLan.GameRuntime.Hash(officialAsi)==ScblLan.GameRuntime.Hash(Path.Combine(system,"scripts/SplinterCellBlacklist.FusionFix.asi")) && ScblLan.GameRuntime.Hash(officialIni)==ScblLan.GameRuntime.Hash(ini),"Official and MOD receive identical embedded binaries and default INI");
        File.SetLastWriteTimeUtc(officialLoader,time);File.SetLastWriteTimeUtc(officialAsi,time);
        var repeatedOfficial=await service.EnsureForLaunchAsync(official,"Official");
        Check(repeatedOfficial.Outcome==FusionFixDeploymentOutcome.AlreadyPresent && File.GetLastWriteTimeUtc(officialLoader)==time && File.GetLastWriteTimeUtc(officialAsi)==time,"Official repeat skips identical binaries without timestamp changes");
        File.WriteAllText(officialIni,"synthetic Official custom INI");
        using(var locked=new FileStream(officialIni,FileMode.Open,FileAccess.ReadWrite,FileShare.None)){
            var kept=await service.EnsureForLaunchAsync(official,"Official");
            using var reader=new StreamReader(locked,leaveOpen:true);
            Check(kept.Outcome==FusionFixDeploymentOutcome.AlreadyPresent && reader.ReadToEnd()=="synthetic Official custom INI","Official preserves exclusively locked existing INI");
        }
        var switched=await service.EnsureForLaunchAsync(official,"MOD");
        Check(switched.Outcome==FusionFixDeploymentOutcome.AlreadyPresent && File.ReadAllText(officialIni)=="synthetic Official custom INI" && File.GetLastWriteTimeUtc(officialLoader)==time,"switching Official to MOD preserves existing configuration and identical binaries");
        File.Delete(officialIni);var completedOfficial=await service.EnsureForLaunchAsync(official,"Official");
        Check(completedOfficial.InstalledFileCount==1 && ScblLan.GameRuntime.Hash(officialIni)==ScblLan.GameRuntime.Hash(ini) && File.GetLastWriteTimeUtc(officialLoader)==time,"Official fills only missing INI without rewriting matching binaries");
        File.WriteAllText(officialLoader,"synthetic Official older loader");string officialOldHash=ScblLan.GameRuntime.Hash(officialLoader);
        var officialReplaced=await service.EnsureForLaunchAsync(official,"Official");
        Check(officialReplaced.ReplacedFileCount==1 && ScblLan.GameRuntime.Hash(officialLoader)==ScblLan.GameRuntime.Hash(loader),"Official replaces a differing loader with packaged bytes");
        Check(officialReplaced.BackupDirectory!=null && Directory.EnumerateFiles(officialReplaced.BackupDirectory,"version.dll",SearchOption.AllDirectories).Any(p=>ScblLan.GameRuntime.Hash(p)==officialOldHash),"Official preserves the old loader in an exact backup");
        File.WriteAllText(report,string.Join("\n",output)+"\nSYNTHETIC_ONLY\n");
    }
}

using SplinterCellCNLauncher.Network;
namespace SplinterCellCNLauncher.Services;
internal static class ReleaseDeclaration
{
    // Construct new messages from the verified immutable snapshot; callers cannot mutate the cached identity.
    internal static PackageIdentity Declaration(this ReleasePackage value)
    {
        ReleaseComponent Role()=>new(){ReleaseId=value.ReleaseId,CompatId=value.CompatId};
        return new(){Schema=1,ReleaseId=value.ReleaseId,CompatId=value.CompatId,ManifestSha256=value.ManifestSha256,Launcher=Role(),MainDll=Role(),GameplayDll=Role(),Server=Role()};
    }
}

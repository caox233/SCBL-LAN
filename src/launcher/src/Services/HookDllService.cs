using ScblLan;
namespace SplinterCellCNLauncher.Services;
public sealed class HookDllService
{
    public void DeployHookDllSafely(string gameDir,string gameExe) => GameRuntime.Deploy(Path.Combine(gameDir,gameExe),RuntimePaths.Root);
}

using System;
using System.IO;
using System.Linq;
using System.Reflection;

namespace SplinterCellCNLauncher.Services;

public static class EmbeddedResourceService
{
    public static void ExtractEmbeddedFileStrict(string fileName, string targetPath)
    {
        string resourceName = FindEmbeddedResourceNameStrict(fileName);

        string? dir = Path.GetDirectoryName(targetPath);
        if (!string.IsNullOrWhiteSpace(dir))
            Directory.CreateDirectory(dir);

        using Stream source = OpenEmbeddedResourceStreamStrict(resourceName);
        using var target = new FileStream(targetPath, FileMode.Create, FileAccess.Write, FileShare.None);
        source.CopyTo(target);
    }

    public static Stream OpenEmbeddedFileStrict(string fileName)
    {
        string resourceName = FindEmbeddedResourceNameStrict(fileName);
        return OpenEmbeddedResourceStreamStrict(resourceName);
    }

    public static bool EmbeddedFileExists(string fileName)
    {
        Assembly assembly = Assembly.GetExecutingAssembly();
        return assembly
            .GetManifestResourceNames()
            .Any(x => IsEmbeddedFileNameMatch(x, fileName));
    }

    private static string FindEmbeddedResourceNameStrict(string fileName)
    {
        Assembly assembly = Assembly.GetExecutingAssembly();

        string? resourceName = assembly
            .GetManifestResourceNames()
            .FirstOrDefault(x => IsEmbeddedFileNameMatch(x, fileName));

        if (resourceName == null)
        {
            string all = string.Join(Environment.NewLine, assembly.GetManifestResourceNames());
            throw new Exception(
                $"内置资源不存在：{fileName}{Environment.NewLine}" +
                $"请确认该文件已放入项目 EmbeddedFiles 目录，并设置为 EmbeddedResource。{Environment.NewLine}" +
                $"当前可见资源：{Environment.NewLine}{all}");
        }

        return resourceName;
    }

    private static Stream OpenEmbeddedResourceStreamStrict(string resourceName)
    {
        Assembly assembly = Assembly.GetExecutingAssembly();
        Stream? source = assembly.GetManifestResourceStream(resourceName);
        if (source == null)
            throw new Exception($"无法读取内置资源：{resourceName}");

        return source;
    }

    private static bool IsEmbeddedFileNameMatch(string resourceName, string fileName)
    {
        return resourceName.EndsWith($".EmbeddedFiles.{fileName}", StringComparison.OrdinalIgnoreCase)
            || resourceName.EndsWith($".{fileName}", StringComparison.OrdinalIgnoreCase)
            || resourceName.Contains(".EmbeddedFiles.", StringComparison.OrdinalIgnoreCase)
                && resourceName.EndsWith(fileName, StringComparison.OrdinalIgnoreCase);
    }
}

using System.IO.Compression;
using System.Security.Cryptography;

namespace SplinterCellCNLauncher.Services;

public enum FusionFixDeploymentOutcome
{
    SkippedOfficial,
    Installed,
    AlreadyPresent,
    ExistingModPreserved
}

public sealed record FusionFixDeploymentResult(
    FusionFixDeploymentOutcome Outcome,
    int InstalledFileCount,
    int PreservedFileCount = 0,
    string ReasonCode = "none",
    string? ConflictRelativePath = null,
    int ReplacedFileCount = 0,
    string? BackupDirectory = null);

/// <summary>
/// Aligns the two managed FusionFix binaries with the frozen payload, retaining
/// byte-for-byte backups before automatic replacement. A missing FusionFix INI
/// is seeded from the frozen asset; an existing player-owned INI is never opened,
/// copied, modified, or deleted during deployment.
/// Existing configuration is identified by name only; existing binaries are
/// reused only when their SHA256 matches the frozen payload. LAN also rejects
/// reparse-point destinations and serializes same-game installation transactions. The embedded source asset
/// itself remains bound to its frozen SHA256.
/// </summary>
public sealed class FusionFixDeploymentService
{
    public const string AssetFileName = "FusionFix.zip";
    public const string AssetSha256 = "B807D01B849CA7AEAD6C04A43EAA2924809D0741E7007B907E00DA868F33A7FF";
    public const string ProtectedIniRelativePath = "SYSTEM/scripts/SplinterCellBlacklist.FusionFix.ini";

    private readonly Func<Stream> _openAsset;
    private readonly string _expectedAssetSha256;
    private readonly Action<FusionFixDeploymentResult> _reportResult;
    private readonly Action<string>? _beforeCommit; // Synthetic failure injection; production is null.

    public FusionFixDeploymentService()
        : this(
            () => typeof(FusionFixDeploymentService).Assembly.GetManifestResourceStream("SCBL.LAN.FusionFix.zip")
                ?? throw new InvalidDataException("Embedded offline FusionFix asset is missing."),
            AssetSha256)
    {
    }

    internal FusionFixDeploymentService(
        Func<Stream> openAsset,
        string expectedAssetSha256,
        Action<FusionFixDeploymentResult>? reportResult = null,
        Action<string>? beforeCommit = null)
    {
        _openAsset = openAsset ?? throw new ArgumentNullException(nameof(openAsset));
        _expectedAssetSha256 = NormalizeSha256(expectedAssetSha256);
        _reportResult = reportResult ?? LogResult;
        _beforeCommit = beforeCommit;
    }

    public Task<FusionFixDeploymentResult> EnsureForLaunchAsync(
        string gameDirectory, string gameplayMode, CancellationToken cancellationToken = default)
    {
        if (!string.Equals(gameplayMode, "MOD", StringComparison.OrdinalIgnoreCase) &&
            !string.Equals(gameplayMode, "Official", StringComparison.OrdinalIgnoreCase))
            throw new ArgumentException("Unknown gameplay mode.", nameof(gameplayMode));
        return EnsureInstalledAsync(gameDirectory, cancellationToken);
    }

    public Task<FusionFixDeploymentResult> EnsureInstalledAsync(
        string gameDirectory,
        CancellationToken cancellationToken = default)
        => Task.Run(
            () => EnsureInstalled(gameDirectory, cancellationToken),
            cancellationToken);

    private FusionFixDeploymentResult EnsureInstalled(
        string gameDirectory,
        CancellationToken cancellationToken)
    {
        string gameRoot = NormalizeGameRoot(gameDirectory);
        EnsureSafePath(gameRoot, Path.Combine(gameRoot, "SYSTEM"));
        string identity = Convert.ToHexString(SHA256.HashData(System.Text.Encoding.UTF8.GetBytes(gameRoot.ToUpperInvariant())));
        using var gate = new Mutex(false, "Local\\SCBL_LAN_FusionFix_" + identity);
        bool owned = false;
        try
        {
            try { owned = gate.WaitOne(0); } catch (AbandonedMutexException) { owned = true; }
            if (!owned) throw new IOException("Another FusionFix installation is already in progress for this game.");
            return InstallCore(gameRoot, cancellationToken);
        }
        finally { if (owned) gate.ReleaseMutex(); }
    }

    private FusionFixDeploymentResult InstallCore(string gameRoot, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        byte[] assetBytes;
        using (Stream source = _openAsset())
        using (var buffer = new MemoryStream())
        {
            source.CopyTo(buffer);
            assetBytes = buffer.ToArray();
        }

        string actualSha256 = Convert.ToHexString(SHA256.HashData(assetBytes));
        if (!actualSha256.Equals(_expectedAssetSha256, StringComparison.OrdinalIgnoreCase))
        {
            throw new InvalidDataException(
                $"FusionFix asset hash mismatch: expected={_expectedAssetSha256}, actual={actualSha256}.");
        }

        using var archiveBuffer = new MemoryStream(assetBytes, writable: false);
        using var archive = new ZipArchive(archiveBuffer, ZipArchiveMode.Read, leaveOpen: false);
        IReadOnlyList<InstallEntry> entries = BuildInstallPlan(archive, gameRoot);
        if (entries.Count == 0)
            throw new InvalidDataException("FusionFix asset contains no installable files below src/.");

        ExistingModInspection existing = InspectExistingMod(gameRoot, entries);
        if (existing.State != ExistingModState.IncompleteSafeToInstall)
        {
            FusionFixDeploymentResult result = new(
                existing.State == ExistingModState.CompleteFrozenPayload
                    ? FusionFixDeploymentOutcome.AlreadyPresent
                    : FusionFixDeploymentOutcome.ExistingModPreserved,
                InstalledFileCount: 0,
                PreservedFileCount: existing.ExistingFileCount,
                ReasonCode: existing.ReasonCode,
                ConflictRelativePath: existing.ConflictRelativePath);
            _reportResult(result);
            return result;
        }

        cancellationToken.ThrowIfCancellationRequested();
        string stagingRoot = Path.Combine(
            gameRoot,
            ".scbl-staging",
            "fusionfix-" + Guid.NewGuid().ToString("N"));
        string backupRoot = Path.Combine(gameRoot, ".scbl-backups", "fusionfix", Guid.NewGuid().ToString("N"));
        IReadOnlyList<InstallEntry> installEntries = existing.MissingEntries;
        var committed = new List<CommittedFile>(installEntries.Count);
        int preservedDuringInstall = 0;
        Exception? primaryError = null;
        var rollbackErrors = new List<Exception>();

        try
        {
            EnsureSafePath(gameRoot, stagingRoot);
            foreach (InstallEntry item in installEntries)
            {
                cancellationToken.ThrowIfCancellationRequested();
                string stagedPath = Path.Combine(stagingRoot, item.RelativePath);
                EnsureSafePath(gameRoot, stagedPath);
                Directory.CreateDirectory(Path.GetDirectoryName(stagedPath)!);
                using Stream input = item.Entry.Open();
                using var output = new FileStream(
                    stagedPath,
                    FileMode.CreateNew,
                    FileAccess.Write,
                    FileShare.None);
                input.CopyTo(output);
                output.Flush(flushToDisk: true);
                if (output.Length != item.Entry.Length)
                    throw new InvalidDataException($"FusionFix staged size mismatch: {item.RelativePath}.");
            }

            foreach (InstallEntry item in installEntries)
            {
                cancellationToken.ThrowIfCancellationRequested();
                string stagedPath = Path.Combine(stagingRoot, item.RelativePath);
                _beforeCommit?.Invoke(item.TargetPath);
                EnsureSafePath(gameRoot, item.TargetPath);
                if (item.OriginalSha256 != null)
                {
                    // Only the two named FusionFix binaries may be replaced. A change
                    // after inspection is retried on the next launch, never clobbered.
                    if (!File.Exists(item.TargetPath) || !MatchesFrozenBinary(item.TargetPath, item.OriginalSha256))
                        throw new IOException("FusionFix target changed during preparation: " + NormalizeRelativePath(item.RelativePath));
                    string backup = Path.Combine(backupRoot, item.RelativePath);
                    EnsureSafePath(gameRoot, backup);
                    Directory.CreateDirectory(Path.GetDirectoryName(backup)!);
                    File.Replace(stagedPath, item.TargetPath, backup);
                    committed.Add(new CommittedFile(item, backup));
                    if (!MatchesFrozenBinary(backup, item.OriginalSha256))
                        throw new IOException("FusionFix backup differs from the inspected original: " + NormalizeRelativePath(item.RelativePath));
                    continue;
                }
                if (File.Exists(item.TargetPath))
                {
                    if (item.IsPlayerEditable)
                    {
                        File.Delete(stagedPath);
                        preservedDuringInstall++;
                        continue;
                    }
                    throw new IOException(
                        $"A MOD file appeared during FusionFix installation; no existing file was replaced: {item.RelativePath}.");
                }

                Directory.CreateDirectory(Path.GetDirectoryName(item.TargetPath)!);
                File.Move(stagedPath, item.TargetPath, overwrite: false);
                committed.Add(new CommittedFile(item, null));
            }

            VerifyInstalledState(entries);
            int replaced = committed.Count(item => item.BackupPath != null);
            FusionFixDeploymentResult result = new(
                FusionFixDeploymentOutcome.Installed,
                InstalledFileCount: committed.Count,
                PreservedFileCount: existing.ExistingFileCount + preservedDuringInstall,
                ReasonCode: replaced > 0 ? "replaced-different-managed-binaries" : existing.ExistingFileCount + preservedDuringInstall == 0
                    ? "installed-frozen-payload"
                    : "completed-missing-files",
                ReplacedFileCount: replaced,
                BackupDirectory: replaced > 0 ? backupRoot : null);
            _reportResult(result);
            return result;
        }
        catch (Exception ex)
        {
            primaryError = ex;
            throw;
        }
        finally
        {
            if (primaryError != null)
            {
                for (int index = committed.Count - 1; index >= 0; index--)
                {
                    try
                    {
                        CommittedFile mutation = committed[index];
                        InstallEntry created = mutation.Entry;
                        EnsureSafePath(gameRoot, created.TargetPath);
                        if (File.Exists(created.TargetPath))
                        {
                            if (!MatchesFrozenBinary(created.TargetPath, created.ExpectedSha256))
                                throw new IOException("Rollback preserved a concurrently changed file: " + NormalizeRelativePath(created.RelativePath));
                            if (mutation.BackupPath == null) File.Delete(created.TargetPath);
                        }
                        if (mutation.BackupPath != null) RestoreReplacedFile(gameRoot, mutation);
                    }
                    catch (Exception rollbackError)
                    {
                        rollbackErrors.Add(rollbackError);
                    }
                }
            }

            try
            {
                if (Directory.Exists(stagingRoot))
                {
                    EnsureSafeTree(gameRoot, stagingRoot);
                    Directory.Delete(stagingRoot, recursive: true);
                }
            }
            catch (Exception cleanupError)
            {
                if (primaryError != null)
                    rollbackErrors.Add(cleanupError);
            }

            if (primaryError != null && rollbackErrors.Count > 0)
            {
                throw new AggregateException(
                    "FusionFix installation failed and rollback was incomplete.",
                    [primaryError, .. rollbackErrors]);
            }
        }
    }

    private static IReadOnlyList<InstallEntry> BuildInstallPlan(
        ZipArchive archive,
        string gameRoot)
    {
        var result = new List<InstallEntry>();
        var targets = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        string rootPrefix = gameRoot.EndsWith(Path.DirectorySeparatorChar)
            ? gameRoot
            : gameRoot + Path.DirectorySeparatorChar;

        foreach (ZipArchiveEntry entry in archive.Entries)
        {
            if (((entry.ExternalAttributes >> 16) & 0xF000) == 0xA000)
                throw new InvalidDataException("Symbolic links are not allowed in the FusionFix payload.");
            string fullName = entry.FullName;
            if (fullName.EndsWith('/'))
                continue;
            if (!fullName.StartsWith("src/", StringComparison.Ordinal)
                || fullName.Contains('\\'))
            {
                throw new InvalidDataException($"FusionFix archive entry is outside src/: {fullName}.");
            }

            string relativeArchivePath = fullName[4..];
            string[] segments = relativeArchivePath.Split('/');
            if (segments.Length == 0
                || segments.Any(segment => string.IsNullOrWhiteSpace(segment)
                    || segment is "." or ".." || IsUnsafeSegment(segment)))
            {
                throw new InvalidDataException($"FusionFix archive entry is not canonical: {fullName}.");
            }

            string normalizedRelativePath = string.Join('/', segments);
            // Include the frozen default INI in the plan. Like other editable
            // configuration, it is created only when absent; existing player
            // files are neither opened nor replaced, including a commit-time race.

            string relativePath = Path.Combine(segments);
            string targetPath = Path.GetFullPath(Path.Combine(gameRoot, relativePath));
            if (!targetPath.StartsWith(rootPrefix, StringComparison.OrdinalIgnoreCase)
                || !targets.Add(targetPath))
            {
                throw new InvalidDataException($"FusionFix archive target is unsafe or duplicated: {fullName}.");
            }

            EnsureSafePath(gameRoot, targetPath);
            if (Directory.Exists(targetPath))
                throw new IOException("A directory occupies a FusionFix file destination: " + normalizedRelativePath);
            bool isPlayerEditable = IsPlayerEditableConfiguration(relativePath);
            if (!isPlayerEditable && !IsManagedBinary(normalizedRelativePath))
                throw new InvalidDataException("FusionFix binary is outside the explicit managed allowlist: " + normalizedRelativePath);
            using Stream frozen = entry.Open();
            string expectedSha256 = Convert.ToHexString(SHA256.HashData(frozen));
            result.Add(new InstallEntry(
                entry,
                relativePath,
                targetPath,
                isPlayerEditable, expectedSha256));
        }

        return result;
    }

    private static ExistingModInspection InspectExistingMod(
        string gameRoot,
        IReadOnlyList<InstallEntry> entries)
    {
        var missingEntries = new List<InstallEntry>();
        int existingFileCount = 0;
        foreach (InstallEntry entry in entries)
        {
            if (!File.Exists(entry.TargetPath))
            {
                missingEntries.Add(entry);
                continue;
            }

            existingFileCount++;
            if (entry.IsPlayerEditable)
                continue;
            if (!MatchesFrozenBinary(entry.TargetPath, entry.ExpectedSha256))
            {
                missingEntries.Add(entry with { OriginalSha256 = HashFile(entry.TargetPath) });
                existingFileCount--;
            }
        }

        return missingEntries.Count == 0
            ? new ExistingModInspection(
                ExistingModState.CompleteFrozenPayload,
                missingEntries,
                existingFileCount,
                ReasonCode: "complete-frozen-payload",
                ConflictRelativePath: null)
            : new ExistingModInspection(
                ExistingModState.IncompleteSafeToInstall,
                missingEntries,
                existingFileCount,
                ReasonCode: "missing-install-files",
                ConflictRelativePath: null);
    }

    private static bool MatchesFrozenBinary(string path, string expectedSha256)
        => HashFile(path).Equals(expectedSha256, StringComparison.OrdinalIgnoreCase);

    private static string HashFile(string path)
    {
        using var input = new FileStream(
            path,
            FileMode.Open,
            FileAccess.Read,
            FileShare.Read | FileShare.Delete);
        return Convert.ToHexString(SHA256.HashData(input));
    }

    private static bool IsManagedBinary(string relativePath)
        => relativePath.Equals("SYSTEM/version.dll", StringComparison.OrdinalIgnoreCase)
            || relativePath.Equals("SYSTEM/scripts/SplinterCellBlacklist.FusionFix.asi", StringComparison.OrdinalIgnoreCase);

    private static void RestoreReplacedFile(string gameRoot, CommittedFile mutation)
    {
        InstallEntry entry = mutation.Entry;
        string backup = mutation.BackupPath!;
        EnsureSafePath(gameRoot, backup);
        if (!File.Exists(backup) || !MatchesFrozenBinary(backup, entry.OriginalSha256!))
            throw new IOException("FusionFix rollback backup is unavailable or changed; retained for inspection.");
        string temporary = entry.TargetPath + ".scbl-restore-" + Guid.NewGuid().ToString("N");
        try
        {
            EnsureSafePath(gameRoot, temporary);
            File.Copy(backup, temporary, overwrite: false);
            if (!MatchesFrozenBinary(temporary, entry.OriginalSha256!))
                throw new IOException("FusionFix rollback copy verification failed.");
            EnsureSafePath(gameRoot, entry.TargetPath);
            if (File.Exists(entry.TargetPath))
            {
                if (!MatchesFrozenBinary(entry.TargetPath, entry.ExpectedSha256))
                    throw new IOException("FusionFix rollback preserved a concurrently changed target.");
                File.Replace(temporary, entry.TargetPath, null);
            }
            else File.Move(temporary, entry.TargetPath, overwrite: false);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
    }

    private static void VerifyInstalledState(IReadOnlyList<InstallEntry> entries)
    {
        foreach (InstallEntry entry in entries)
        {
            if (!File.Exists(entry.TargetPath))
            {
                throw new InvalidDataException(
                    $"FusionFix install target is missing after deployment: {NormalizeRelativePath(entry.RelativePath)}.");
            }
            if (!entry.IsPlayerEditable && !MatchesFrozenBinary(entry.TargetPath, entry.ExpectedSha256))
            {
                throw new InvalidDataException(
                    $"FusionFix binary differs from its frozen payload after deployment: {NormalizeRelativePath(entry.RelativePath)}.");
            }
        }
    }

    private static bool IsPlayerEditableConfiguration(string relativePath)
        => Path.GetExtension(relativePath).ToLowerInvariant() is
            ".ini" or ".xml" or ".toml" or ".json" or ".cfg" or ".conf";

    private static bool IsUnsafeSegment(string segment)
    {
        if (segment.EndsWith('.') || segment.EndsWith(' ') || segment.IndexOfAny(Path.GetInvalidFileNameChars()) >= 0)
            return true;
        string stem = segment.Split('.')[0].ToUpperInvariant();
        return stem is "CON" or "PRN" or "AUX" or "NUL" or "CLOCK$"
            || (stem.Length == 4 && (stem.StartsWith("COM") || stem.StartsWith("LPT")) && stem[3] is >= '0' and <= '9');
    }

    private static void CheckNotReparse(string path)
    {
        try
        {
            if ((File.GetAttributes(path) & FileAttributes.ReparsePoint) != 0)
                throw new IOException("FusionFix refuses a symbolic-link or junction destination.");
        }
        catch (FileNotFoundException) { }
        catch (DirectoryNotFoundException) { }
    }

    private static void EnsureSafePath(string root, string target)
    {
        string full = Path.GetFullPath(target);
        string prefix = root.TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
        if (!full.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            throw new IOException("FusionFix destination escapes the selected game root.");
        for (DirectoryInfo? parent = new(root); parent != null; parent = parent.Parent)
            CheckNotReparse(parent.FullName);
        string current = root;
        foreach (string segment in Path.GetRelativePath(root, full).Split(Path.DirectorySeparatorChar))
        {
            current = Path.Combine(current, segment);
            CheckNotReparse(current);
        }
    }

    private static void EnsureSafeTree(string root, string directory)
    {
        EnsureSafePath(root, directory);
        foreach (string path in Directory.EnumerateFileSystemEntries(directory))
        {
            EnsureSafePath(root, path);
            if (Directory.Exists(path)) EnsureSafeTree(root, path);
        }
    }

    private static string NormalizeRelativePath(string relativePath)
        => relativePath.Replace(Path.DirectorySeparatorChar, '/');

    private static void LogResult(FusionFixDeploymentResult result)
    {
        string message =
            $"FusionFix deployment: outcome={result.Outcome}, reason={result.ReasonCode}, " +
            $"installed={result.InstalledFileCount}, replaced={result.ReplacedFileCount}, preserved={result.PreservedFileCount}" +
            (string.IsNullOrWhiteSpace(result.ConflictRelativePath)
                ? "."
                : $", conflict={result.ConflictRelativePath}.");
        if (result.Outcome == FusionFixDeploymentOutcome.ExistingModPreserved)
            LogService.Info("FusionFix conflict: " + message);
        else
            LogService.Info(message);
    }

    private static string NormalizeGameRoot(string gameDirectory)
    {
        if (string.IsNullOrWhiteSpace(gameDirectory))
            throw new ArgumentException("A game directory is required.", nameof(gameDirectory));
        string root = Path.GetFullPath(gameDirectory);
        if (!Directory.Exists(root))
            throw new DirectoryNotFoundException($"Game directory does not exist: {root}");
        root = root.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);

        var selected = new DirectoryInfo(root);
        if (selected.Name.Equals("SYSTEM", StringComparison.OrdinalIgnoreCase)
            && selected.Parent != null)
        {
            // Launcher settings intentionally point at src/SYSTEM because the
            // game executable lives there. The FusionFix archive is rooted at
            // src/, so installation must start from the parent src directory.
            return selected.Parent.FullName.TrimEnd(
                Path.DirectorySeparatorChar,
                Path.AltDirectorySeparatorChar);
        }

        string nestedSourceRoot = Path.Combine(root, "src");
        if (Directory.Exists(Path.Combine(nestedSourceRoot, "SYSTEM")))
            return Path.GetFullPath(nestedSourceRoot);

        return root;
    }

    private static string NormalizeSha256(string value)
    {
        string normalized = value?.Trim() ?? "";
        if (normalized.Length != 64 || normalized.Any(ch => !Uri.IsHexDigit(ch)))
            throw new ArgumentException("A 64-character SHA256 is required.", nameof(value));
        return normalized.ToUpperInvariant();
    }

    private sealed record InstallEntry(
        ZipArchiveEntry Entry,
        string RelativePath,
        string TargetPath,
        bool IsPlayerEditable, string ExpectedSha256, string? OriginalSha256 = null);

    private sealed record CommittedFile(InstallEntry Entry, string? BackupPath);

    private sealed record ExistingModInspection(
        ExistingModState State,
        IReadOnlyList<InstallEntry> MissingEntries,
        int ExistingFileCount,
        string ReasonCode,
        string? ConflictRelativePath);

    private enum ExistingModState
    {
        IncompleteSafeToInstall,
        CompleteFrozenPayload,
        ConflictingModPreserved
    }
}

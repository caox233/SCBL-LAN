# Bootstrap only: no installs, elevation, registry writes, game launch or upload.
param([Parameter(ValueFromRemainingArguments=$true)][string[]]$BuildArgs)
$ErrorActionPreference = 'Stop'
try {
    $root = Split-Path -Parent $PSScriptRoot
    Set-Location -LiteralPath $root
    $env:PYTHONUTF8 = '1'
    $env:PYTHONDONTWRITEBYTECODE = '1'

    # CMD drops oversized inherited PATH values. Recover installed tool paths,
    # removing stale SDK folders only from this child process, never the registry.
    $paths = @($env:PATH)
    if (-not $env:PATH) {
        $paths += [Environment]::GetEnvironmentVariable('Path', 'Machine')
        $paths += [Environment]::GetEnvironmentVariable('Path', 'User')
    }
    $seen = @{}
    $valid = @()
    foreach ($value in $paths) {
        foreach ($part in ($value -split ';')) {
            if (-not $part) { continue }
            $path = [Environment]::ExpandEnvironmentVariables($part.Trim('"'))
            if (-not $seen.ContainsKey($path) -and (Test-Path -LiteralPath $path -PathType Container)) {
                $seen[$path] = $true
                $valid += $path
            }
        }
    }
    $env:PATH = $valid -join ';'
    Write-Host 'SCBL LAN - check, test, build, package and verify'
    Write-Host 'No game launch, system installation or publishing.'
    $python = $null
    $prefix = @()
    foreach ($name in @('py.exe', 'python.exe')) {
        $command = Get-Command $name -CommandType Application -ErrorAction SilentlyContinue
        if (-not $command) { continue }
        $arguments = @()
        if ($name -eq 'py.exe') { $arguments = @('-3') }
        & $command.Source @arguments -c 'import sys; sys.exit(0 if sys.version_info >= (3,11) else 1)' 2>$null
        if ($LASTEXITCODE -eq 0) { $python = $command.Source; $prefix = $arguments; break }
    }
    if (-not $python) {
        throw 'Python 3.11+ not found. Install from https://www.python.org/downloads/windows/ then reopen build.cmd. See docs/BUILD.md.'
    }
    & $python @prefix -X utf8 -B (Join-Path $PSScriptRoot 'build.py') all @BuildArgs
    exit $LASTEXITCODE
} catch {
    [Console]::Error.WriteLine('ERROR: ' + $_.Exception.Message)
    exit 1
}

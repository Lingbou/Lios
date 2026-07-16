$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$helper = (Resolve-Path "$PSScriptRoot\..\src-tauri\windows\path-helper.ps1").Path
$windowsPowerShell = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
$testRoot = "Software\Lios\Tests\PathHelper-$([guid]::NewGuid().ToString('N'))"
$longPathKey = "$testRoot\LongPath"
$emptyPathKey = "$testRoot\EmptyPath"
$installDirectory = "C:\Lios Path Smoke"

function Invoke-PathHelper([string] $Action, [string] $RegistrySubKey) {
    & $windowsPowerShell `
        -NoLogo `
        -NoProfile `
        -NonInteractive `
        -ExecutionPolicy Bypass `
        -File $helper `
        $Action `
        $installDirectory `
        $RegistrySubKey
    if ($LASTEXITCODE -ne 0) {
        throw "path helper $Action failed with exit code $LASTEXITCODE"
    }
}

function Read-RawUserPath([string] $RegistrySubKey) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($RegistrySubKey)
    if ($null -eq $key) {
        return $null
    }
    try {
        return $key.GetValue(
            "Path",
            $null,
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
        )
    }
    finally {
        $key.Dispose()
    }
}

try {
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($longPathKey)
    try {
        $entries = 0..90 | ForEach-Object {
            "C:\Synthetic\PathSegment$($_.ToString('D3'))\NestedDirectory"
        }
        $original = "%USERPROFILE%\bin;$($entries -join ';');"
        if ($original.Length -le 2048) {
            throw "synthetic PATH is not long enough"
        }
        $key.SetValue(
            "Path",
            $original,
            [Microsoft.Win32.RegistryValueKind]::ExpandString
        )
    }
    finally {
        $key.Dispose()
    }

    Invoke-PathHelper "add" $longPathKey
    $added = [string] (Read-RawUserPath $longPathKey)
    if (-not $added.StartsWith($original, [StringComparison]::Ordinal)) {
        throw "long PATH prefix changed while adding Lios"
    }
    if (-not $added.EndsWith($installDirectory, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Lios install directory was not appended"
    }

    Invoke-PathHelper "add" $longPathKey
    if ([string] (Read-RawUserPath $longPathKey) -cne $added) {
        throw "adding Lios to PATH is not idempotent"
    }

    Invoke-PathHelper "remove" $longPathKey
    if ([string] (Read-RawUserPath $longPathKey) -cne $original) {
        throw "long PATH was not restored byte-for-byte"
    }

    [void] [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($emptyPathKey).Dispose()
    Invoke-PathHelper "add" $emptyPathKey
    Invoke-PathHelper "remove" $emptyPathKey
    $emptyKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($emptyPathKey)
    try {
        if ($emptyKey.GetValueNames() -contains "Path") {
            throw "removing the only PATH entry should remove the registry value"
        }
    }
    finally {
        $emptyKey.Dispose()
    }

    Write-Output "Windows PATH helper preserved a $($original.Length)-character PATH"
}
finally {
    [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree($testRoot, $false)
}

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet("add", "remove")]
    [string] $Action,

    [Parameter(Mandatory = $true, Position = 1)]
    [ValidateNotNullOrEmpty()]
    [string] $InstallDirectory,

    [Parameter(Position = 2)]
    [ValidateNotNullOrEmpty()]
    [string] $RegistrySubKey = "Environment"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Normalize-PathEntry {
    param([AllowNull()] [string] $Value)

    if ($null -eq $Value) {
        return ""
    }
    $trimmed = $Value.Trim().Trim([char]'"')
    if ($trimmed.Length -eq 0) {
        return ""
    }
    try {
        return [IO.Path]::GetFullPath($trimmed).TrimEnd([char]'\')
    }
    catch {
        return $trimmed.TrimEnd([char]'\')
    }
}

$normalizedInstallDirectory = Normalize-PathEntry $InstallDirectory
if ($normalizedInstallDirectory.Length -eq 0) {
    throw "install directory must not be empty"
}

$registry = [Microsoft.Win32.Registry]::CurrentUser
$key = $registry.OpenSubKey($RegistrySubKey, $true)
if ($null -eq $key) {
    if ($Action -eq "remove") {
        return
    }
    $key = $registry.CreateSubKey($RegistrySubKey)
}

try {
    $rawValue = $key.GetValue(
        "Path",
        $null,
        [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
    )
    $rawPath = if ($null -eq $rawValue) { "" } else { [string] $rawValue }
    $entries = if ($rawPath.Length -eq 0) {
        @()
    }
    else {
        @($rawPath.Split([char]';'))
    }
    $comparer = [StringComparer]::OrdinalIgnoreCase

    if ($Action -eq "add") {
        foreach ($entry in $entries) {
            if ($comparer.Equals((Normalize-PathEntry $entry), $normalizedInstallDirectory)) {
                return
            }
        }
        $updatedPath = if ($rawPath.Length -eq 0) {
            $normalizedInstallDirectory
        }
        else {
            "$rawPath;$normalizedInstallDirectory"
        }
        $key.SetValue(
            "Path",
            $updatedPath,
            [Microsoft.Win32.RegistryValueKind]::ExpandString
        )
        return
    }

    $remainingEntries = [Collections.Generic.List[string]]::new()
    $removed = $false
    foreach ($entry in $entries) {
        if ($comparer.Equals((Normalize-PathEntry $entry), $normalizedInstallDirectory)) {
            $removed = $true
        }
        else {
            $remainingEntries.Add($entry)
        }
    }
    if (-not $removed) {
        return
    }

    $updatedPath = $remainingEntries -join ";"
    if ($updatedPath.Length -eq 0) {
        $key.DeleteValue("Path", $false)
    }
    else {
        $key.SetValue(
            "Path",
            $updatedPath,
            [Microsoft.Win32.RegistryValueKind]::ExpandString
        )
    }
}
finally {
    if ($null -ne $key) {
        $key.Dispose()
    }
}

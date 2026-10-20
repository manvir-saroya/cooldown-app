<#
  Smoke test for the Windows installer: install it silently, start the installed app hidden, check
  its log file, stop it, uninstall it. Fails (exit 1) if any step does not hold.

  Used by CI (windows.yml and release.yml). It can also be run on a throwaway Windows machine:
    pwsh .github/scripts/windows-smoke-test.ps1 -Installer C:\path\Cooldown_0.1.0_x64-setup.exe

  Names are derived from apps/desktop/src-tauri/tauri.conf.json (productName, mainBinaryName,
  identifier, version). The NSIS installer is a per-user install (installMode "currentUser"), so
  nothing here needs administrator rights.

  The log file comes from tauri-plugin-log (apps/desktop/src-tauri/src/lib.rs: LogDir, file name
  "cooldown"). Tauri's app_log_dir on Windows is %LOCALAPPDATA%\<identifier>\logs.
#>
param(
    [Parameter(Mandatory = $true)][string]$Installer,
    # How long the app runs before its log is judged.
    [int]$WaitSeconds = 20
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$conf = Get-Content -Raw (Join-Path $repoRoot 'apps\desktop\src-tauri\tauri.conf.json') | ConvertFrom-Json
$productName = $conf.productName
$identifier = $conf.identifier
$version = $conf.version
# The installed main binary is mainBinaryName if the config sets one, otherwise the Cargo binary
# (cooldown-desktop; the installer does not rename it to productName).
$cargoToml = Get-Content -Raw (Join-Path $repoRoot 'apps\desktop\src-tauri\Cargo.toml')
$cargoBin = if ($cargoToml -match '(?ms)\[\[bin\]\]\s*\r?\nname\s*=\s*"([^"]+)"') { $Matches[1] } else { $null }
$mainBinary = if ($conf.PSObject.Properties['mainBinaryName'] -and $conf.mainBinaryName) { $conf.mainBinaryName } elseif ($cargoBin) { $cargoBin } else { $productName }

$logDir = Join-Path $env:LOCALAPPDATA "$identifier\logs"
$logFile = Join-Path $logDir 'cooldown.log'

$Installer = (Resolve-Path $Installer).Path
Write-Host "Installer : $Installer ($([math]::Round((Get-Item $Installer).Length / 1MB, 2)) MiB)"
Write-Host "Product   : $productName $version (identifier $identifier, main binary $mainBinary.exe)"
Write-Host "Log file  : $logFile"

$script:installDir = $null
$script:exe = $null
$script:proc = $null

function Stop-Cooldown {
    # Stops the app we started and anything else running from the install directory.
    if ($script:proc -and -not $script:proc.HasExited) {
        & taskkill.exe /PID $script:proc.Id /T /F | Out-Null
    }
    if ($script:installDir) {
        Get-Process -ErrorAction SilentlyContinue | Where-Object {
            $_.Path -and $_.Path.StartsWith($script:installDir, [System.StringComparison]::OrdinalIgnoreCase)
        } | ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
    }
}

function Show-Log {
    if (Test-Path $logFile) {
        Write-Host "::group::cooldown.log"
        Get-Content $logFile | Write-Host
        Write-Host "::endgroup::"
    } else {
        Write-Host "(no log file at $logFile)"
        $appData = Join-Path $env:LOCALAPPDATA $identifier
        if (Test-Path $appData) {
            Write-Host "Contents of ${appData}:"
            Get-ChildItem -Recurse -Force $appData | ForEach-Object { Write-Host "  $($_.FullName)" }
        } else {
            Write-Host "($appData does not exist)"
        }
    }
}

function Fail([string]$message) {
    Write-Host "::error::$message"
    Show-Log
    throw $message
}

function Find-InstallDir {
    # 1. The uninstall entry the installer registers for the current user.
    $uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall'
    if (Test-Path $uninstallKey) {
        foreach ($key in Get-ChildItem $uninstallKey) {
            $props = Get-ItemProperty $key.PSPath
            if ($props.PSObject.Properties['DisplayName'] -and $props.DisplayName -eq $productName) {
                Write-Host "Uninstall entry: $($key.PSChildName)"
                $props | Format-List DisplayName, DisplayVersion, Publisher, InstallLocation, UninstallString | Out-String | Write-Host
                # The installer stores these values with quotation marks around them.
                $location = if ($props.PSObject.Properties['InstallLocation'] -and $props.InstallLocation) { ($props.InstallLocation -replace '"', '').Trim() } else { $null }
                if ($location -and (Test-Path $location)) {
                    return (Resolve-Path $location).Path
                }
                if ($props.PSObject.Properties['UninstallString'] -and $props.UninstallString) {
                    $uninstallExe = ($props.UninstallString -replace '"', '').Trim()
                    if (Test-Path $uninstallExe) { return (Split-Path -Parent (Resolve-Path $uninstallExe).Path) }
                }
            }
        }
    }
    # 2. NSIS defaults for a per-user install.
    foreach ($candidate in @("$env:LOCALAPPDATA\$productName", "$env:LOCALAPPDATA\Programs\$productName")) {
        if (Test-Path $candidate) { return $candidate }
    }
    return $null
}

function Find-Exe([string]$dir) {
    foreach ($name in @("$mainBinary.exe", "$productName.exe")) {
        $candidate = Join-Path $dir $name
        if (Test-Path $candidate) { return $candidate }
    }
    # Last resort: the only executable that is neither the uninstaller nor a bundled sidecar.
    $others = @(Get-ChildItem $dir -Filter *.exe -File | Where-Object { $_.Name -notmatch '^(uninstall|cooldown-statusline)' })
    if ($others.Count -eq 1) { return $others[0].FullName }
    return $null
}

$failed = $false
try {
    # A clean slate, so everything found in the log below comes from this run.
    if (Test-Path $logDir) { Remove-Item -Recurse -Force $logDir }

    Write-Host "::group::Install (silent)"
    $install = Start-Process -FilePath $Installer -ArgumentList '/S' -Wait -PassThru
    Write-Host "Installer exit code: $($install.ExitCode)"
    if ($install.ExitCode -ne 0) { Fail "The installer exited with code $($install.ExitCode)" }

    $script:installDir = Find-InstallDir
    if (-not $script:installDir) { Fail "Could not find where $productName was installed" }
    Write-Host "Install directory: $($script:installDir)"
    Get-ChildItem -Force $script:installDir | ForEach-Object { Write-Host "  $($_.Name)  ($($_.Length) bytes)" }
    Write-Host "::endgroup::"

    $script:exe = Find-Exe $script:installDir
    if (-not $script:exe) { Fail "No $mainBinary.exe in $($script:installDir)" }
    $uninstaller = Join-Path $script:installDir 'uninstall.exe'
    if (-not (Test-Path $uninstaller)) { Fail "No uninstall.exe in $($script:installDir)" }

    # The installer may have started the app itself; end that copy and clear its log.
    Stop-Cooldown
    Start-Sleep -Seconds 2
    if (Test-Path $logDir) { Remove-Item -Recurse -Force $logDir }

    Write-Host "Starting $($script:exe) --hidden"
    $script:proc = Start-Process -FilePath $script:exe -ArgumentList '--hidden' -PassThru
    $deadline = (Get-Date).AddSeconds($WaitSeconds)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 1
        if ($script:proc.HasExited) { Fail "Cooldown exited by itself with code $($script:proc.ExitCode) after starting" }
    }
    Write-Host "Cooldown has been running for $WaitSeconds s (pid $($script:proc.Id))"

    if (-not (Test-Path $logFile)) { Fail "The log file was not created: $logFile" }
    $log = Get-Content -Raw $logFile
    Show-Log
    if ($log -notmatch ("Cooldown " + [regex]::Escape($version) + " starting")) {
        Fail "The log does not say 'Cooldown $version starting'"
    }
    if ($log -match '(?i)panic') { Fail "The log mentions a panic" }
    if ($log -match '(?i)startup failed') { Fail "The log says startup failed" }
    Write-Host "Smoke test passed: the app started, logged 'starting' and logged no panic or startup failure."
} catch {
    $failed = $true
    Write-Host "::error::$($_.Exception.Message)"
} finally {
    Stop-Cooldown
}

# Uninstall even after a failure, so a broken run does not leave the runner dirty.
if ($script:installDir) {
    Write-Host "::group::Uninstall (silent)"
    $uninstaller = Join-Path $script:installDir 'uninstall.exe'
    if (Test-Path $uninstaller) {
        # "_?=<dir>" makes the NSIS uninstaller run in place, so -Wait really waits (normally it
        # copies itself to %TEMP% and returns at once). The cost: uninstall.exe stays behind.
        $un = Start-Process -FilePath $uninstaller -ArgumentList '/S', "_?=$($script:installDir)" -Wait -PassThru
        Write-Host "Uninstaller exit code: $($un.ExitCode)"
        if ($un.ExitCode -ne 0) { Write-Host "::error::The uninstaller exited with code $($un.ExitCode)"; $failed = $true }
    }
    if ($script:exe -and (Test-Path $script:exe)) {
        Write-Host "::error::$($script:exe) is still there after uninstalling"
        $failed = $true
    } else {
        Write-Host "The application files were removed."
    }
    Write-Host "::endgroup::"
}

if ($failed) { exit 1 }
exit 0

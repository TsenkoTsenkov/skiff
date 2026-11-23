# Builds Skiff for Windows and packages it two ways, into dist\:
#
#   Skiff-windows-x64.zip         portable: skiff.exe and the licence
#   Skiff-windows-x64-setup.exe   installer for the current user (no
#                                 administrator rights; Start menu shortcut,
#                                 uninstaller, "Installed apps" entry)
#
#   pwsh hack\windows-package.ps1                 build and package
#   pwsh hack\windows-package.ps1 -Exe <file>     package this skiff.exe, do not build
#   pwsh hack\windows-package.ps1 -NoInstaller    only the zip (no NSIS needed)
#
# Needs Rust with the MSVC toolchain and, for the installer, NSIS 3
# (winget install NSIS.NSIS, or choco install nsis).
#
# Signing is optional. Without the variables below both files are unsigned
# and Windows shows the SmartScreen warning on first start. With them,
# skiff.exe is signed before it is packed and the installer after it is
# made (packaging\README.md says where each value comes from):
#
#   Azure Artifact Signing (formerly Trusted Signing):
#     AZURE_TENANT_ID, AZURE_CLIENT_ID, AZURE_CLIENT_SECRET   the service principal
#     AZURE_SIGNING_ENDPOINT   e.g. https://weu.codesigning.azure.net
#     AZURE_SIGNING_ACCOUNT    the signing account's name
#     AZURE_SIGNING_PROFILE    the certificate profile's name
#   A certificate file:
#     WINDOWS_CERT_PFX_BASE64  the .pfx, base64-encoded
#     WINDOWS_CERT_PASSWORD    its password
#
# Other environment:
#   SKIFF_BUILD        build number (git rev-list --count HEAD); 0 locally
#   SKIFF_UPDATE_REPO  owner/repo the update check asks, when not the default
[CmdletBinding()]
param(
    [string]$Target = 'x86_64-pc-windows-msvc',
    [string]$Exe = '',
    [string]$OutDir = 'dist',
    [switch]$NoInstaller
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# Runs a program and stops the script when it fails (PowerShell does not do
# that by itself for programs).
function Invoke-Checked {
    param([string]$Program, [string[]]$Arguments)
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Program failed with exit code $LASTEXITCODE"
    }
}

function Get-EnvValue {
    param([string]$Name)
    $value = [Environment]::GetEnvironmentVariable($Name)
    if ($null -eq $value) { return '' }
    return $value.Trim()
}

# ---- which way to sign, if any ---------------------------------------------

$azureNames = @('AZURE_TENANT_ID', 'AZURE_CLIENT_ID', 'AZURE_CLIENT_SECRET',
    'AZURE_SIGNING_ENDPOINT', 'AZURE_SIGNING_ACCOUNT', 'AZURE_SIGNING_PROFILE')
$azureMissing = @($azureNames | Where-Object { (Get-EnvValue $_) -eq '' })
$signMode = 'none'
if ($azureMissing.Count -eq 0) {
    $signMode = 'azure'
}
elseif ((Get-EnvValue 'WINDOWS_CERT_PFX_BASE64') -ne '') {
    $signMode = 'pfx'
}
elseif (@($azureNames | Where-Object { $_ -like 'AZURE_SIGNING_*' -and (Get-EnvValue $_) -ne '' }).Count -gt 0) {
    # Half-configured signing would silently ship an unsigned release.
    throw "Azure signing is only partly configured; missing: $($azureMissing -join ', ')"
}

function Find-SignTool {
    $found = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($found) { return $found.Source }
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $candidates = @(Get-ChildItem -Path $kits -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -like '*\x64\*' } | Sort-Object FullName -Descending)
    if ($candidates.Count -eq 0) {
        throw 'signtool.exe was not found (it comes with the Windows SDK).'
    }
    return $candidates[0].FullName
}

$pfxPath = ''
function Invoke-Sign {
    param([string]$File)
    switch ($script:signMode) {
        'none' { return }
        'azure' {
            # Microsoft's module; it reads the AZURE_* credentials from the
            # environment and calls signtool with the signing service.
            if (-not (Get-Module -ListAvailable -Name TrustedSigning)) {
                Install-Module -Name TrustedSigning -Scope CurrentUser -Force -Repository PSGallery
            }
            Invoke-TrustedSigning `
                -Endpoint (Get-EnvValue 'AZURE_SIGNING_ENDPOINT') `
                -CodeSigningAccountName (Get-EnvValue 'AZURE_SIGNING_ACCOUNT') `
                -CertificateProfileName (Get-EnvValue 'AZURE_SIGNING_PROFILE') `
                -Files $File `
                -FileDigest SHA256 `
                -TimestampRfc3161 'http://timestamp.acs.microsoft.com' `
                -TimestampDigest SHA256
        }
        'pfx' {
            if ($script:pfxPath -eq '') {
                $script:pfxPath = Join-Path ([IO.Path]::GetTempPath()) ("skiff-sign-" + [Guid]::NewGuid().ToString('N') + '.pfx')
                [IO.File]::WriteAllBytes($script:pfxPath, [Convert]::FromBase64String((Get-EnvValue 'WINDOWS_CERT_PFX_BASE64')))
            }
            Invoke-Checked (Find-SignTool) @('sign', '/f', $script:pfxPath, '/p', (Get-EnvValue 'WINDOWS_CERT_PASSWORD'),
                '/fd', 'SHA256', '/tr', 'http://timestamp.digicert.com', '/td', 'SHA256', $File)
        }
    }
    # Whatever signed it: the result must carry a valid signature.
    $signature = Get-AuthenticodeSignature -FilePath $File
    if ($signature.Status -ne 'Valid') {
        throw "$File is not validly signed after signing: $($signature.Status) $($signature.StatusMessage)"
    }
    Write-Host "Signed $File ($($signature.SignerCertificate.Subject))"
}

try {
    # ---- build ---------------------------------------------------------------

    if ($Exe -eq '') {
        Invoke-Checked 'cargo' @('build', '--release', '--locked', '-p', 'skiff', '--target', $Target)
        $Exe = Join-Path $root "target\$Target\release\skiff.exe"
    }
    if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) {
        throw "no such file: $Exe"
    }
    $Exe = (Resolve-Path -LiteralPath $Exe).Path

    $versionLine = Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.*)"' | Select-Object -First 1
    if (-not $versionLine) { throw 'no version in Cargo.toml' }
    $version = $versionLine.Matches[0].Groups[1].Value
    # The installer's file version is four numbers: x.y.z.build.
    if ($version -notmatch '^\d+\.\d+\.\d+$') {
        throw "the version in Cargo.toml must be x.y.z for the Windows installer, not '$version'"
    }
    $build = Get-EnvValue 'SKIFF_BUILD'
    if ($build -notmatch '^\d+$') { $build = '0' }

    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
    $OutDir = (Resolve-Path -LiteralPath $OutDir).Path
    $stage = Join-Path ([IO.Path]::GetTempPath()) ("skiff-package-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $stage | Out-Null

    # The files that are shipped: a copy, so that signing does not touch the
    # build output.
    $stagedExe = Join-Path $stage 'skiff.exe'
    Copy-Item -LiteralPath $Exe -Destination $stagedExe
    Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination (Join-Path $stage 'LICENSE.txt')
    Invoke-Sign $stagedExe

    # ---- portable zip --------------------------------------------------------

    $zip = Join-Path $OutDir 'Skiff-windows-x64.zip'
    if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip }
    Compress-Archive -Path $stagedExe, (Join-Path $stage 'LICENSE.txt') -DestinationPath $zip -CompressionLevel Optimal
    Write-Host "Built $zip"

    # ---- installer -----------------------------------------------------------

    if (-not $NoInstaller) {
        $makensis = Get-Command makensis.exe -ErrorAction SilentlyContinue
        if ($makensis) {
            $makensis = $makensis.Source
        }
        else {
            $makensis = Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe'
            if (-not (Test-Path -LiteralPath $makensis)) {
                throw 'NSIS is not installed (winget install NSIS.NSIS), or pass -NoInstaller.'
            }
        }
        $setup = Join-Path $OutDir 'Skiff-windows-x64-setup.exe'
        if (Test-Path -LiteralPath $setup) { Remove-Item -LiteralPath $setup }
        $nsisArgs = @(
            '/V2',
            "/DVERSION=$version",
            "/DBUILD=$build",
            "/DEXE=$stagedExe",
            "/DLICENSE=$(Join-Path $root 'LICENSE')",
            "/DOUTFILE=$setup"
        )
        $icon = Join-Path $root 'crates\skiff\assets\icons\skiff.ico'
        if (Test-Path -LiteralPath $icon) {
            $nsisArgs += "/DICON=$icon"
        }
        else {
            Write-Warning "$icon is missing (hack/icons.sh makes it); the installer gets the NSIS icon"
        }
        $nsisArgs += (Join-Path $root 'packaging\windows\skiff.nsi')
        Invoke-Checked $makensis $nsisArgs
        if (-not (Test-Path -LiteralPath $setup)) { throw "makensis did not write $setup" }
        # (The uninstaller inside the installer stays unsigned; Windows does
        # not check it when it is started from "Installed apps".)
        Invoke-Sign $setup
        Write-Host "Built $setup"
    }

    if ($signMode -eq 'none') {
        Write-Host 'UNSIGNED: no signing variables are set.'
    }
}
finally {
    if ($pfxPath -ne '' -and (Test-Path -LiteralPath $pfxPath)) { Remove-Item -LiteralPath $pfxPath -Force }
    if ((Get-Variable -Name stage -ErrorAction SilentlyContinue) -and (Test-Path -LiteralPath $stage)) {
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
}

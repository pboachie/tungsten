<#
.SYNOPSIS
Install the Tungsten CLI from a GitHub Release.

.DESCRIPTION
Installs the tungsten CLI from a GitHub Release on Windows (x86_64 or ARM64). No
administrator rights are needed.

  irm https://raw.githubusercontent.com/pboachie/tungsten/main/install.ps1 | iex
  & ([scriptblock]::Create((irm https://raw.githubusercontent.com/pboachie/tungsten/main/install.ps1))) -Init
  .\install.ps1 [-Version v0.1.2] [-InstallDir DIR] [-Init]

  -Init          after installing, run the guided `tungsten init` setup; needs
                 terminal input. The `irm ... | iex` form cannot pass parameters:
                 use the scriptblock form above, or save the script and run
                 .\install.ps1 -Init
  -Version       release tag (default: the latest release; TUNGSTEN_VERSION)
  -InstallDir    target directory (default: %LOCALAPPDATA%\Programs\tungsten\bin;
                 TUNGSTEN_INSTALL_DIR)
  -BaseUrl       directory URL laid out like GitHub's releases/download
                 (<base>/<tag>/<asset>), for mirrors and tests; needs -Version
                 (TUNGSTEN_RELEASE_BASE_URL)
  TUNGSTEN_REPO  owner/name of the repository (default: pboachie/tungsten)
  TUNGSTEN_REQUIRE_ATTESTATION  1: explicitly require the default provenance policy
  TUNGSTEN_NO_ATTESTATION       1: choose checksum-only installation (conflicts with REQUIRE=1)
  TUNGSTEN_LIBC                 Linux install.sh only: musl (default) or gnu;
                               not a Windows archive selector

The archive is checked against the release's SHA256SUMS before anything is
installed. Official release-workflow provenance is required by default,
including for mirrors; GitHub CLI 2.102.0+ is required. The directory is not
added to PATH; the script prints the command.

Exit status (when run as a script file): 0 installed (and, with -Init, set up);
1 nothing installed or the install failed; 4 installed, but the -Init setup was
cancelled (or refused); 5 installed, but the -Init setup failed. The in-memory
forms set $LASTEXITCODE instead of closing the session.
#>
[CmdletBinding()]
param(
    [switch]$Init,
    [string]$Version = $env:TUNGSTEN_VERSION,
    [string]$InstallDir = $env:TUNGSTEN_INSTALL_DIR,
    [string]$BaseUrl = $env:TUNGSTEN_RELEASE_BASE_URL
)

if ($Init -and [Console]::IsInputRedirected) { throw '-Init requires terminal input; install without -Init, then run tungsten init interactively' }

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Fail([string]$Message) {
    throw "install.ps1: error: $Message"
}

function Invoke-GhAttestation([string[]]$Arguments, [string]$OutputFile, [string]$ErrorFile) {
    # Windows PowerShell can otherwise turn native stderr into a terminating
    # error before we inspect the exit status. Always decide from that status.
    $ErrorActionPreference = 'Continue'
    $PSNativeCommandUseErrorActionPreference = $false
    & gh @Arguments 1> $OutputFile 2> $ErrorFile
    return $LASTEXITCODE
}

function Assert-AttestationSettings {
    foreach ($name in @('TUNGSTEN_NO_ATTESTATION', 'TUNGSTEN_REQUIRE_ATTESTATION')) {
        $value = [Environment]::GetEnvironmentVariable($name)
        if ($value -and $value -notin @('0', '1')) { Fail "$name must be 0 or 1" }
    }
    if ($env:TUNGSTEN_NO_ATTESTATION -eq '1' -and $env:TUNGSTEN_REQUIRE_ATTESTATION -eq '1') {
        Fail 'TUNGSTEN_NO_ATTESTATION=1 conflicts with TUNGSTEN_REQUIRE_ATTESTATION=1'
    }
}

function Require-AttestationVerifier {
    if ($env:TUNGSTEN_NO_ATTESTATION -eq '1') { return }
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
        Fail 'build provenance is required: install GitHub CLI 2.102.0 or newer from https://cli.github.com; TUNGSTEN_NO_ATTESTATION=1 explicitly chooses checksum-only installation'
    }
    $output = Join-Path $work 'attestation-out.log'
    $errorFile = Join-Path $work 'attestation-error.log'
    $versionCode = Invoke-GhAttestation @('--version') $output $errorFile
    $versionLine = Get-Content -LiteralPath $output -TotalCount 1
    if ($versionCode -ne 0 -or $versionLine -notmatch '^gh version (\d+\.\d+\.\d+)(?: |$)' -or [version]$Matches[1] -lt [version]'2.102.0') {
        Fail 'GitHub CLI 2.102.0 or newer is required for exact workflow attestation verification'
    }
    if ((Invoke-GhAttestation @('attestation', 'verify', '--help') $output $errorFile) -ne 0) {
        Get-Content -LiteralPath $errorFile -Tail 8 | ForEach-Object { Write-Host $_ }
        Fail 'gh attestation verify is unavailable; install GitHub CLI 2.102.0 or newer'
    }
}

function Verify-Attestation([string]$Archive, [string]$Tag) {
    if ($env:TUNGSTEN_NO_ATTESTATION -eq '1') {
        Write-Host 'install.ps1: WARNING: checksum-only installation requested; build provenance not verified (TUNGSTEN_NO_ATTESTATION=1)'
        return
    }
    $output = Join-Path $work 'attestation-out.log'
    $errorFile = Join-Path $work 'attestation-error.log'
    # Mirror/repository download overrides never select the trusted signer.
    $arguments = @('attestation', 'verify', $Archive, '--hostname', 'github.com', '--repo', 'pboachie/tungsten',
        '--signer-workflow', 'pboachie/tungsten/.github/workflows/release.yml',
        '--cert-oidc-issuer', 'https://token.actions.githubusercontent.com', '--source-ref', "refs/tags/$Tag")
    if ((Invoke-GhAttestation $arguments $output $errorFile) -ne 0) {
        Get-Content -LiteralPath $errorFile -Tail 8 | ForEach-Object { Write-Host $_ }
        Fail 'the official build attestation could not be verified; nothing was installed'
    }
    Write-Host 'install.ps1: official build attestation verified'
}

Assert-AttestationSettings
$repo = if ($env:TUNGSTEN_REPO) { $env:TUNGSTEN_REPO } else { 'pboachie/tungsten' }
$build = "from source with: cargo install --git https://github.com/$repo tungsten-cli"

$arch = $env:PROCESSOR_ARCHITEW6432
if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
# PowerShell 7 reports the native OS architecture even under x64 emulation.
if ('System.Runtime.InteropServices.RuntimeInformation' -as [type]) {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
}
switch ($arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    'X64' { $target = 'x86_64-pc-windows-msvc' }
    'ARM64' { $target = 'aarch64-pc-windows-msvc' }
    default { Fail "unsupported CPU architecture: $arch (releases cover x86_64 and ARM64 on Windows)" }
}

if (-not $InstallDir) {
    if (-not $env:LOCALAPPDATA) { Fail 'LOCALAPPDATA is not set; pass -InstallDir' }
    $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\tungsten\bin'
}

if (-not $BaseUrl) {
    $BaseUrl = "https://github.com/$repo/releases/download"
    if (-not $Version) {
        $latest = "https://github.com/$repo/releases/latest"
        try {
            $response = Invoke-WebRequest -Uri $latest -MaximumRedirection 5 -UseBasicParsing
            $final = $response.BaseResponse.ResponseUri.AbsoluteUri
            if (-not $final) { $final = $response.BaseResponse.RequestMessage.RequestUri.AbsoluteUri }
        } catch {
            Fail "no latest release; see https://github.com/$repo/releases; build $build"
        }
        $Version = ($final -split '/')[-1]
    }
} elseif (-not $Version) {
    Fail 'a custom -BaseUrl needs -Version'
}
if ($Version -notmatch '^v?\d') { Fail "invalid version: $Version" }
if ($Version -notmatch '^[A-Za-z0-9._+-]+$') { Fail "invalid version: $Version" }
if (-not $Version.StartsWith('v')) { $Version = "v$Version" }

$name = "tungsten-$($Version.Substring(1))-$target"
$asset = "$name.zip"
$work = Join-Path ([IO.Path]::GetTempPath()) ("tungsten-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    Require-AttestationVerifier
    Write-Host "install.ps1: installing tungsten $Version for $target"
    $sums = Join-Path $work 'SHA256SUMS'
    try {
        Invoke-WebRequest -Uri "$BaseUrl/$Version/SHA256SUMS" -OutFile $sums -UseBasicParsing
    } catch {
        Fail "no release $Version (no SHA256SUMS); see https://github.com/$repo/releases; build $build"
    }
    $zip = Join-Path $work $asset
    try {
        Invoke-WebRequest -Uri "$BaseUrl/$Version/$asset" -OutFile $zip -UseBasicParsing
    } catch {
        Fail "release $Version has no archive for $target ($asset)"
    }

    $line = Get-Content $sums | Where-Object { ($_ -split '\s+', 2)[1] -in @($asset, "*$asset") } | Select-Object -First 1
    if (-not $line) { Fail "$asset is not listed in SHA256SUMS" }
    $expected = ($line -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLowerInvariant()
    if ($expected -ne $actual) {
        Fail "checksum mismatch for $asset (expected $expected, got $actual); nothing was installed"
    }
    Write-Host 'install.ps1: checksum ok'

    Verify-Attestation $zip $Version

    Expand-Archive -Path $zip -DestinationPath $work -Force
    $exe = Join-Path $work "$name\tungsten.exe"
    if (-not (Test-Path $exe)) { Fail "unexpected archive layout in $asset" }
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $staged = Join-Path $InstallDir '.tungsten.exe.new'
    Copy-Item -Path $exe -Destination $staged -Force
    Move-Item -Path $staged -Destination (Join-Path $InstallDir 'tungsten.exe') -Force
    $installed = Join-Path $InstallDir 'tungsten.exe'
    Write-Host "install.ps1: installed $installed"
    $onPath = ($env:PATH -split ';') -contains $InstallDir
    if (-not $onPath) {
        Write-Host "install.ps1: note: $InstallDir is not on your PATH; add it for your user with:"
        Write-Host "  [Environment]::SetEnvironmentVariable('Path', `"$InstallDir;`" + [Environment]::GetEnvironmentVariable('Path','User'), 'User')"
    }
    if ($Init) {
        & $installed init --interactive
        $setup = $LASTEXITCODE
        if ($setup -ne 0) {
            if ($setup -eq 4) {
                $code = 4
                Write-Host "install.ps1: setup cancelled (or refused; see above): tungsten is installed; run $installed init when ready"
            } else {
                $code = 5
                Write-Host "install.ps1: setup failed (tungsten init exited $setup): tungsten is installed; correct the problem and run $installed init again"
            }
            $global:LASTEXITCODE = $code
            # A saved script reports the code; an in-memory scriptblock must not close the session.
            if ($PSCommandPath) { exit $code }
            return
        }
    }
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

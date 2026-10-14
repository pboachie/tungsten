<#
Installs the tungsten CLI from a GitHub Release on Windows (x86_64 or ARM64). No
administrator rights are needed.

  irm https://raw.githubusercontent.com/pboachie/tungsten/main/install.ps1 | iex
  & ([scriptblock]::Create((irm https://raw.githubusercontent.com/pboachie/tungsten/main/install.ps1))) -Init
  .\install.ps1 [-Version v0.1.0] [-InstallDir DIR] [-Init]

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

The archive is checked against the release's SHA256SUMS before anything is
installed, and against its build attestation when the GitHub CLI (gh) is
available. The directory is not added to PATH; the script prints the command.

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
            Fail "no release found: $latest does not exist yet. Releases are not published until v0.1.0; build $build"
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
    Write-Host "install.ps1: installing tungsten $Version for $target"
    $sums = Join-Path $work 'SHA256SUMS'
    try {
        Invoke-WebRequest -Uri "$BaseUrl/$Version/SHA256SUMS" -OutFile $sums -UseBasicParsing
    } catch {
        Fail "no release $Version found at $BaseUrl/$Version/ (no SHA256SUMS). Releases are not published until v0.1.0; build $build"
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

    if ($BaseUrl -ne "https://github.com/$repo/releases/download") {
        Write-Host 'install.ps1: custom release location: skipping the build attestation check'
    } elseif (Get-Command gh -ErrorAction SilentlyContinue) {
        & gh attestation verify $zip --repo $repo *> $null
        if ($LASTEXITCODE -ne 0) { Fail "the build attestation of $asset could not be verified; nothing was installed" }
        Write-Host 'install.ps1: build attestation verified'
    } else {
        Write-Host 'install.ps1: gh is not installed: build attestation not checked (checksum only)'
    }

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

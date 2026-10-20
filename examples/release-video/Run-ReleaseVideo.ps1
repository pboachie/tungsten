# SPDX-License-Identifier: AGPL-3.0-only
param(
  [Parameter(Mandatory=$true)][string]$Version,
  [string]$RunId = 'review-001',
  [ValidateSet('audit','prepare','narrate','record','render','qa','compact','package','all')][string]$Stage = 'all',
  [ValidateSet('am_fenrir','am_michael','am_puck')][string]$Voice = 'am_fenrir',
  [string]$Profile,
  [switch]$AllowRegistry
)
$ErrorActionPreference = 'Stop'
$releaseArgs = @((Join-Path $PSScriptRoot 'release_video.py'),'--version',$Version,'--run-id',$RunId,'--stage',$Stage,'--voice',$Voice)
if ($Profile) { $releaseArgs += @('--profile',$Profile) }
if ($AllowRegistry) { $releaseArgs += '--allow-registry' }
$releasePython = Join-Path $PSScriptRoot '.work\.venv\Scripts\python.exe'
if (-not (Test-Path -LiteralPath $releasePython)) { $releasePython = 'python' }
& $releasePython @releaseArgs
if ($LASTEXITCODE -ne 0) { throw "Release video stage failed with exit $LASTEXITCODE" }

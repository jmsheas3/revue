# The Windows version of smoke-test.sh, the smallest proof that a revue binary works: it reports the version in
# Cargo.toml and the commit it was built from (and not "modified"), starts on a repo with one commit and one edit,
# and answers /api/info, /api/tree and the page.
# Usage: smoke-test.ps1 -Binary path\to\revue.exe
param([Parameter(Mandatory = $true)][string]$Binary)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Fail([string]$Message) {
  Write-Output "::error title=Smoke test::$Message"
  throw $Message
}

# Native commands don't stop on failure by themselves.
function Invoke-Git {
  & git @args
  if ($LASTEXITCODE -ne 0) { Fail "git $args exited with $LASTEXITCODE" }
}

$bin = (Resolve-Path $Binary).Path
$root = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$expected = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.*)"$' |
  Select-Object -First 1).Matches[0].Groups[1].Value
$head = (& git -C $root rev-parse HEAD | Out-String).Trim()

$work = Join-Path ([IO.Path]::GetTempPath()) ('revue-smoke-' + [guid]::NewGuid().ToString('N'))
$repo = Join-Path $work 'smoke'
New-Item -ItemType Directory -Path $repo | Out-Null
$proc = $null
try {
  Invoke-Git init -q -b main $repo
  [IO.File]::WriteAllText((Join-Path $repo 'hello.txt'), "hello`n")
  Invoke-Git -C $repo add hello.txt
  Invoke-Git -C $repo -c user.name=smoke -c user.email=smoke@example.com commit -qm first
  [IO.File]::WriteAllText((Join-Path $repo 'hello.txt'), "hello, world`n")

  $version = (& $bin --version | Out-String).Trim()
  Write-Output $version
  if ($version -notmatch '^revue (\S+) \(([0-9a-f]+)\)$') { Fail "--version printed '$version', not 'revue <version> (<commit>)'" }
  if ($Matches[1] -ne $expected) { Fail "--version says $($Matches[1]), Cargo.toml says $expected" }
  if (-not $head.StartsWith($Matches[2])) { Fail "--version says commit $($Matches[2]), this checkout is $head" }

  $out = Join-Path $work 'out.log'
  $err = Join-Path $work 'err.log'
  $proc = Start-Process -FilePath $bin -ArgumentList @("`"$repo`"", '--no-open', '--port', '0') -PassThru -NoNewWindow `
    -RedirectStandardOutput $out -RedirectStandardError $err
  $url = $null
  $deadline = (Get-Date).AddSeconds(30)
  while (-not $url) {
    $m = Select-String -Path $out -Pattern 'http://127\.0\.0\.1:\d+/' -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($m) { $url = $m.Matches[0].Value; break }
    if ($proc.HasExited) { Fail "revue exited with $($proc.ExitCode): $(Get-Content -Raw $out) $(Get-Content -Raw $err)" }
    if ((Get-Date) -gt $deadline) { Fail "revue didn't print its address within 30 s: $(Get-Content -Raw $err)" }
    Start-Sleep -Milliseconds 200
  }
  Write-Output "listening at $url"

  $info = Invoke-RestMethod -Uri "${url}api/info" -TimeoutSec 30
  if (-not ($info.git -eq $true -and $info.name -eq 'smoke' -and $info.branch -eq 'main' -and $info.hasHead)) {
    Fail "GET /api/info answered $($info | ConvertTo-Json -Compress)"
  }
  Write-Output 'ok: the repo'
  $tree = Invoke-RestMethod -Uri "${url}api/tree?mode=working" -TimeoutSec 30
  $files = @($tree.files)
  if (-not ($files.Count -eq 1 -and $files[0] -eq 'hello.txt' -and $tree.changes.'hello.txt'.status -eq 'M')) {
    Fail "GET /api/tree answered $($tree | ConvertTo-Json -Compress -Depth 5)"
  }
  Write-Output 'ok: the edit'
  $page = Invoke-WebRequest -Uri $url -TimeoutSec 30 -UseBasicParsing
  if ($page.Content -notmatch '<html') { Fail "GET / didn't answer the page" }
  Write-Output 'ok: the page'
  Write-Output 'Smoke test passed.'
} finally {
  if ($proc -and -not $proc.HasExited) {
    Stop-Process -Id $proc.Id -Force
    $proc.WaitForExit()
  }
  Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

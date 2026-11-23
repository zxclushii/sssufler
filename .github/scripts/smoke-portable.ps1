param([Parameter(Mandatory)][string]$Executable)
$ErrorActionPreference = 'Stop'

$runDir = Join-Path $env:RUNNER_TEMP ("portable smoke " + [guid]::NewGuid())
$cliDir = New-Item -ItemType Directory -Path (Join-Path $runDir 'cli with spaces') -Force
$exe = (Copy-Item -LiteralPath $Executable -Destination $runDir -PassThru).FullName
$cliExe = Join-Path $cliDir 'claude.exe'
$cliCmd = Join-Path $cliDir 'claude.cmd'
& rustc --edition=2021 --crate-name claude_fixture (Join-Path $PSScriptRoot 'claude-fixture.rs') -o $cliExe
if ($LASTEXITCODE -ne 0) { throw 'Could not compile the terminal fixture' }
@'
@echo off
"%~dp0claude.exe" %*
'@ | Set-Content -LiteralPath $cliCmd -Encoding ascii

$config = Join-Path $runDir 'config.json'
$promptPath = Join-Path $runDir 'claude-prompt.txt'
$marker = Join-Path $runDir 'workspace/launch.txt'

function Assert-TerminalLaunch($process, [string[]]$expected) {
    $deadline = [datetime]::UtcNow.AddSeconds(60)
    while (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
        if ($process.HasExited) { throw "The portable app exited during startup with code $($process.ExitCode)" }
        if ([datetime]::UtcNow -gt $deadline) { throw 'The packaged UI did not start the terminal' }
        Start-Sleep -Milliseconds 150
    }
    $received = [IO.File]::ReadAllText($marker).Split([char]0)
    if ($received.Length -ne $expected.Length) { throw "Unexpected argument count: $($received | ConvertTo-Json -Compress)" }
    for ($i = 0; $i -lt $expected.Length; $i++) {
        if ($expected[$i] -ceq $promptPath -and (Test-Path -LiteralPath $received[$i] -PathType Leaf)) {
            if ([IO.File]::ReadAllText($received[$i]) -ceq 'portable smoke') { continue }
        }
        if ($received[$i] -cne $expected[$i]) { throw "Unexpected argument at index ${i}: [$($received[$i])]" }
    }
    Remove-Item -LiteralPath $marker
}

@{
    claudeCommand = $cliCmd
    claudeArgs = '--model haiku'
    systemPrompt = 'portable smoke'
    whisperUrl = 'http://127.0.0.1:1'
    whisperModel = 'base-q5_1'
    autoSend = $false
} | ConvertTo-Json | Set-Content -LiteralPath $config -Encoding utf8

$baseArgs = @('--model', 'haiku', '--strict-mcp-config', '--append-system-prompt-file', $promptPath, '--tools')
$first = $null
$second = $null
$native = $null
$missing = $null
try {
    $first = Start-Process -FilePath $exe -WorkingDirectory $runDir -PassThru
    Assert-TerminalLaunch $first ($baseArgs + @(''))
    if ([IO.File]::ReadAllText($promptPath) -ne 'portable smoke') { throw 'The portable app did not load the configured prompt' }

    $second = Start-Process -FilePath $exe -WorkingDirectory $runDir -PassThru
    if (-not $second.WaitForExit(10000) -or $first.HasExited) { throw 'Second launch did not leave the original instance running' }
    $first.Refresh()
    if (-not $first.CloseMainWindow() -or -not $first.WaitForExit(15000)) { throw 'The original window did not close cleanly' }
    $saved = Get-Content -LiteralPath $config -Raw | ConvertFrom-Json
    if ($saved.whisperModel -ne 'base-q5_1' -or $saved.autoSend -ne $false -or $saved.bounds.width -lt 380) {
        throw 'The application did not preserve the settings and save the window bounds'
    }
    $saved.claudeCommand = $cliExe
    $saved.webSearch = $true
    $saved | ConvertTo-Json | Set-Content -LiteralPath $config -Encoding utf8
    $native = Start-Process -FilePath $exe -WorkingDirectory $runDir -PassThru
    Assert-TerminalLaunch $native ($baseArgs + @('WebSearch', '--allowedTools', 'WebSearch'))
    $native.Refresh()
    if (-not $native.CloseMainWindow() -or -not $native.WaitForExit(15000)) { throw 'The native CLI test window did not close' }
    Write-Output 'Packaged WebView started both CLI formats with the expected arguments, enforced a single instance and persisted settings.'

    $invalidRuntime = New-Item -ItemType Directory -Path (Join-Path $runDir 'missing-runtime')
    $start = [Diagnostics.ProcessStartInfo]::new($exe)
    $start.UseShellExecute = $false
    $start.WorkingDirectory = $runDir
    $start.EnvironmentVariables['WEBVIEW2_BROWSER_EXECUTABLE_FOLDER'] = $invalidRuntime.FullName
    $missing = [Diagnostics.Process]::Start($start)
    if (-not $missing.WaitForExit(30000) -or $missing.ExitCode -eq 0) {
        throw 'An unavailable WebView2 runtime did not produce a startup failure'
    }
    $log = Get-Content (Join-Path $runDir 'logs/suflyor.log') -Raw
    if ($log -notmatch 'https://developer\.microsoft\.com/microsoft-edge/webview2/') {
        throw 'The startup error did not include the WebView2 installation link'
    }
    Write-Output 'Unavailable WebView2 produced a startup error with the installation link.'
} catch {
    Get-ChildItem (Join-Path $runDir 'logs') -Filter '*.log' -ErrorAction SilentlyContinue |
        Get-Content -Tail 30
    throw
} finally {
    foreach ($proc in @($missing, $native, $second, $first)) {
        if ($null -ne $proc -and -not $proc.HasExited) {
            $proc.Kill()
            $proc.WaitForExit()
        }
    }
}

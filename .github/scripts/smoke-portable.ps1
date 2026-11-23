param([Parameter(Mandatory)][string]$Executable)
$ErrorActionPreference = 'Stop'

$runDir = Join-Path $env:RUNNER_TEMP ("portable smoke " + [guid]::NewGuid())
$cliDir = New-Item -ItemType Directory -Path (Join-Path $runDir 'cli with spaces') -Force
$exe = (Copy-Item -LiteralPath $Executable -Destination $runDir -PassThru).FullName
$cli = Join-Path $cliDir 'claude.cmd'
@'
@echo off
>launch.tmp echo %*
move /y launch.tmp launch.txt >nul
echo terminal ready
'@ | Set-Content -LiteralPath $cli -Encoding ascii

$config = Join-Path $runDir 'config.json'
@{
    claudeCommand = $cli
    claudeArgs = '--model haiku'
    systemPrompt = 'portable smoke'
    whisperUrl = 'http://127.0.0.1:1'
    whisperModel = 'base-q5_1'
    autoSend = $false
} | ConvertTo-Json | Set-Content -LiteralPath $config -Encoding utf8
$first = $null
$second = $null
try {
    $first = Start-Process -FilePath $exe -WorkingDirectory $runDir -PassThru
    $marker = Join-Path $runDir 'workspace/launch.txt'
    $deadline = [datetime]::UtcNow.AddSeconds(60)
    while (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
        if ($first.HasExited) { throw "The portable app exited during startup with code $($first.ExitCode)" }
        if ([datetime]::UtcNow -gt $deadline) { throw 'The packaged UI did not start the terminal' }
        Start-Sleep -Milliseconds 150
    }
    $argsLine = Get-Content -LiteralPath $marker -Raw
    if ($argsLine -notmatch '"--tools" ""' -or $argsLine -notmatch '"--strict-mcp-config"') {
        throw "Unexpected Claude arguments in the portable app: $argsLine"
    }
    if ((Get-Content (Join-Path $runDir 'claude-prompt.txt') -Raw) -ne 'portable smoke') {
        throw 'The portable app did not load the configured prompt'
    }
    $second = Start-Process -FilePath $exe -WorkingDirectory $runDir -PassThru
    if (-not $second.WaitForExit(10000) -or $first.HasExited) { throw 'Second launch did not leave the original instance running' }
    $first.Refresh()
    if (-not $first.CloseMainWindow() -or -not $first.WaitForExit(15000)) { throw 'The original window did not close cleanly' }
    $saved = Get-Content -LiteralPath $config -Raw | ConvertFrom-Json
    if ($saved.whisperModel -ne 'base-q5_1' -or $saved.autoSend -ne $false -or $saved.bounds.width -lt 380) {
        throw 'The application did not preserve the settings and save the window bounds'
    }
    Write-Output 'Packaged WebView started the terminal, reused the original window and persisted settings.'
} catch {
    Get-ChildItem (Join-Path $runDir 'logs') -Filter '*.log' -ErrorAction SilentlyContinue |
        Get-Content -Tail 30
    throw
} finally {
    foreach ($proc in @($second, $first)) {
        if ($null -ne $proc -and -not $proc.HasExited) {
            $proc.Kill()
            $proc.WaitForExit()
        }
    }
}

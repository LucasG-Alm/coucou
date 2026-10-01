<#
.SYNOPSIS
  Fires a fake PermissionRequest through the installed relay and waits for you to
  click Allow or Deny on the island.

.DESCRIPTION
  Tests the whole approval path without needing a real agent to ask for something:
  coucou-hook -> named pipe -> island card -> your click -> back to the relay's
  stdout. Prints exactly what the agent (Claude Code or Codex) would receive.

  Needs Coucou running and its relay installed (Coucou copies it to
  %LOCALAPPDATA%\Coucou\bin when it starts).

  The test makes up a session, so the island shows a pill for it. The script sends
  a SessionEnd at the end to take that pill away again.

.PARAMETER Agent
  Which agent to pretend to be: claude (default) or codex.

.PARAMETER Command
  The command the fake request asks permission for; shown on the card.

.EXAMPLE
  .\perm-test.ps1 -Agent codex

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File .\perm-test.ps1 -Command "git push --force"

.NOTES
  No answer within ~110 s (or a paused island) prints nothing: that is the
  "terminal takes over" case, and it is the correct behaviour.
#>
param(
    [ValidateSet("claude", "codex")]
    [string]$Agent = "claude",
    [string]$Command = "rm -rf node_modules"
)

$hook = Join-Path $env:LOCALAPPDATA "Coucou\bin\coucou-hook.exe"
if (-not (Test-Path $hook)) {
    Write-Error "Relay not found at $hook. Start Coucou once so it installs it."
    exit 1
}

$session = "perm-test-$Agent"
$cwd = (Get-Location).Path

function Send-Event($event, $extra = @{}) {
    $payload = @{ hook_event_name = $event; session_id = $session; cwd = $cwd } + $extra
    $payload | ConvertTo-Json -Compress -Depth 4 | & $hook $event --agent $Agent
}

Write-Host "Pretending to be $Agent. Click Allow or Deny on the island (waits up to ~110 s)..."
$started = Get-Date
$answer = Send-Event "PermissionRequest" @{
    tool_name  = "Bash"
    tool_input = @{ command = $Command; description = "Permission test" }
}
$seconds = [math]::Round(((Get-Date) - $started).TotalSeconds, 1)

if ($answer) {
    Write-Host "Answered after ${seconds}s. The agent would receive:"
    Write-Host $answer
} else {
    Write-Host "No answer after ${seconds}s: the agent would have asked in the terminal."
}

# Take the made-up session's pill away.
Send-Event "SessionEnd" | Out-Null

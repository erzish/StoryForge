# Differential test: compares the Rust backend against the reference Node backend
# for the same database contents.
#
# Usage:
#   pwsh -File scripts/difftest.ps1 [-Rust http://127.0.0.1:5123] [-Node http://127.0.0.1:5199]
#
# Both servers must be running against identical copies of story.db.
# Exit code is the number of mismatching routes (0 = full parity on the probed set).

param(
    [string]$Rust = 'http://127.0.0.1:5123',
    [string]$Node = 'http://127.0.0.1:5199',
    [switch]$Verbose
)

$ErrorActionPreference = 'Stop'

$paths = @(
    '/api/books',
    '/api/books/1',
    '/api/books/1/chapters',
    '/api/books/1/characters',
    '/api/books/1/relationships',
    '/api/books/1/events',
    '/api/books/1/timeline',
    '/api/books/1/promises',
    '/api/books/1/secrets',
    '/api/books/1/world',
    '/api/books/1/moments',
    '/api/books/1/graph',
    '/api/books/1/facts',
    '/api/books/1/knowledge',
    '/api/books/1/diff?from=1&to=5',
    '/api/books/1/state?chapter=3',
    '/api/books/1/state?chapter=3&full=1',
    '/api/dictionaries',
    '/api/characters/1',
    '/api/characters/1/state?chapter=3',
    '/api/chapters/1/text',
    '/api/chapters/1/versions',
    '/api/chapters/1/annotations',
    '/api/books/1/annotations',
    '/api/books/1/activity',
    '/api/books/1/style-examples',
    '/api/books/1/ai/history',
    '/api/ai/settings',
    '/api/books/1/text-search?q=Мия&limit=5',
    '/api/books/1/facts/search?q=Мия&limit=5'
)

function Get-Json($base, $path) {
    try {
        $response = Invoke-WebRequest -Uri ($base + $path) -TimeoutSec 20 -UseBasicParsing
        $body = $response.Content
        $parsed = $null
        try { $parsed = $body | ConvertFrom-Json -AsHashtable -Depth 100 } catch { $parsed = $null }
        return [pscustomobject]@{ Status = $response.StatusCode; Body = $body; Json = $parsed }
    }
    catch [System.Net.WebException] {
        $status = 0
        if ($_.Exception.Response) { $status = [int]$_.Exception.Response.StatusCode }
        $reader = New-Object System.IO.StreamReader($_.Exception.Response.GetResponseStream())
        $text = $reader.ReadToEnd()
        $parsed = $null
        try { $parsed = $text | ConvertFrom-Json -AsHashtable -Depth 100 } catch { }
        return [pscustomobject]@{ Status = $status; Body = $text; Json = $parsed }
    }
    catch {
        return [pscustomobject]@{ Status = -1; Body = $_.Exception.Message; Json = $null }
    }
}

function Compare-Json($a, $b, $pathText) {
    if ($null -eq $a -and $null -eq $b) { return @() }
    if ($null -eq $a -or $null -eq $b) { return @("$pathText : one side is null") }

    if ($a -is [System.Collections.IDictionary]) {
        if (-not ($b -is [System.Collections.IDictionary])) { return @("$pathText : object vs $($b.GetType().Name)") }
        $diffs = @()
        $keysA = $a.Keys | Sort-Object
        $keysB = $b.Keys | Sort-Object
        foreach ($k in $keysA) {
            if (-not $b.ContainsKey($k)) { $diffs += "$pathText.$k : missing on rust side"; continue }
            $diffs += Compare-Json $a[$k] $b[$k] "$pathText.$k"
        }
        foreach ($k in $keysB) {
            if (-not $a.ContainsKey($k)) { $diffs += "$pathText.$k : extra on rust side" }
        }
        return $diffs
    }

    if ($a -is [System.Collections.IList]) {
        if (-not ($b -is [System.Collections.IList])) { return @("$pathText : array vs $($b.GetType().Name)") }
        if ($a.Count -ne $b.Count) { return @("$pathText : length $($a.Count) vs $($b.Count)") }
        $diffs = @()
        for ($i = 0; $i -lt $a.Count; $i++) {
            $diffs += Compare-Json $a[$i] $b[$i] "$pathText[$i]"
        }
        return $diffs
    }

    if ("$a" -ne "$b") { return @("$pathText : '$a' vs '$b'") }
    return @()
}

$failures = 0
foreach ($path in $paths) {
    $rustResult = Get-Json $Rust $path
    $nodeResult = Get-Json $Node $path

    $problems = @()
    if ($rustResult.Status -ne $nodeResult.Status) {
        $problems += "status $($rustResult.Status) vs $($nodeResult.Status)"
    }
    elseif ($null -eq $rustResult.Json -or $null -eq $nodeResult.Json) {
        if ($rustResult.Body -ne $nodeResult.Body) { $problems += 'body differs (non-JSON)' }
    }
    else {
        $problems += Compare-Json $nodeResult.Json $rustResult.Json ''
    }

    if ($problems.Count -gt 0) {
        $failures++
        Write-Host "FAIL $path" -ForegroundColor Red
        $problems | Select-Object -First 12 | ForEach-Object { Write-Host "     $_" -ForegroundColor DarkYellow }
        if ($problems.Count -gt 12) { Write-Host "     … $($problems.Count - 12) more" -ForegroundColor DarkYellow }
    }
    else {
        Write-Host "ok   $path" -ForegroundColor Green
    }
}

Write-Host ''
Write-Host ("routes: {0}   mismatching: {1}" -f $paths.Count, $failures) -ForegroundColor Cyan
exit $failures

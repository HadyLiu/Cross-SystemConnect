# Local CI/CD for Windows. Run via the cmd launcher so ExecutionPolicy does not block it.
#   scripts\local-ci.cmd
#   scripts\local-ci.cmd -Upload -Version v0.1.0
param(
    [switch]$Upload,
    [string]$Version = ""
)

$ErrorActionPreference = "Stop"
Set-Location (Resolve-Path (Join-Path $PSScriptRoot ".."))

$script:failed = $false
$script:results = New-Object System.Collections.Generic.List[object]

function Add-Result([string]$Name, [string]$Result) {
    $script:results.Add([pscustomobject]@{ Check = $Name; Result = $Result }) | Out-Null
}

function Invoke-Checked([string]$Title, [scriptblock]$Action) {
    if ($script:failed) {
        Add-Result $Title "skip"
        return
    }
    Write-Host ""
    Write-Host "== $Title"
    try {
        & $Action
    } catch {
        $script:failed = $true
        Add-Result $Title "fail"
        Write-Host "$_"
        return
    }
    if ($LASTEXITCODE -ne 0) {
        $script:failed = $true
        Add-Result $Title "fail"
        Write-Host "$Title failed (exit $LASTEXITCODE)"
        return
    }
    Add-Result $Title "pass"
}

function Show-Summary {
    Write-Host ""
    Write-Host ("{0,-16} {1}" -f "Check", "Result")
    Write-Host ("{0,-16} {1}" -f "----------------", "------")
    $overall = "pass"
    foreach ($row in $script:results) {
        if ($row.Result -ne "pass") { $overall = "fail" }
        Write-Host ("{0,-16} {1}" -f $row.Check, $row.Result)
    }
    Write-Host ("{0,-16} {1}" -f "overall", $overall)
}

try {
if ($Upload -and $Version -notmatch '^v\d+\.\d+\.\d+$') {
    Write-Host "Upload needs a version like v0.1.0"
    $script:failed = $true
    Add-Result "version" "fail"
    exit 1
}

Invoke-Checked "fmt" { cargo fmt --all -- --check }
Invoke-Checked "test" { cargo test --workspace --all-targets }
Invoke-Checked "clippy" { cargo clippy --workspace --all-targets -- -D clippy::correctness -D clippy::suspicious }

if ($script:failed) {
    Add-Result "audit" "skip"
} elseif (-not (Get-Command cargo-audit -ErrorAction SilentlyContinue)) {
    Write-Host ""
    Write-Host "cargo-audit is missing. Install once: cargo install cargo-audit --locked"
    $script:failed = $true
    Add-Result "audit" "fail"
} else {
    Invoke-Checked "audit" { cargo audit }
}

Invoke-Checked "release build" { cargo build --release -p csc-core -p csc-ui }

if ($script:failed) {
    Add-Result "package" "skip"
} else {
    Write-Host ""
    Write-Host "== package"
    $dist = Join-Path (Get-Location) "dist"
    New-Item -ItemType Directory -Force -Path $dist | Out-Null
    $ui = Join-Path $dist "Cross-SystemConnect-windows-x86_64.exe"
    $core = Join-Path $dist "Cross-SystemConnect-core-windows-x86_64.exe"
    $copied = $false
    try {
        Copy-Item -Force "target\release\Cross-SystemConnect.exe" $ui
        Copy-Item -Force "target\release\Cross-SystemConnect-core.exe" $core
        $copied = (Test-Path $ui) -and (Test-Path $core)
    } catch {
        Write-Host "$_"
        $copied = $false
    }
    if ($copied) {
        Add-Result "package" "pass"
        Write-Host "Packaged:"
        Write-Host "  $ui"
        Write-Host "  $core"
    } else {
        $script:failed = $true
        Add-Result "package" "fail"
    }
}

if ($Upload) {
    if ($script:failed) {
        Add-Result "gh auth" "skip"
        Add-Result "upload" "skip"
    } elseif (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
        Write-Host ""
        Write-Host "GitHub CLI (gh) is missing. Install it and run: gh auth login"
        $script:failed = $true
        Add-Result "gh auth" "fail"
        Add-Result "upload" "skip"
    } else {
        Invoke-Checked "gh auth" { gh auth status }
        if (-not $script:failed) {
            $dirty = git status --porcelain
            if ($dirty) {
                Write-Host ""
                Write-Host "Working tree has uncommitted changes. The uploaded programs include this build; the GitHub tag points at the current commit."
            }
            gh release view $Version *> $null
            if ($LASTEXITCODE -ne 0) {
                Invoke-Checked "upload" { gh release create $Version --title $Version --notes "Built locally." --target HEAD $ui $core }
            } else {
                Invoke-Checked "upload" { gh release upload $Version $ui $core --clobber }
            }
        } else {
            Add-Result "upload" "skip"
        }
    }
}

} catch {
    $script:failed = $true
    Add-Result "script" "fail"
    Write-Host "$_"
} finally {
    Show-Summary
}
if ($script:failed) { exit 1 }
if (-not $Upload) {
    Write-Host ""
    Write-Host "Build only. To upload: scripts\local-ci.cmd -Upload -Version v0.1.0"
}
exit 0

param(
    [Parameter(Mandatory = $true)][string]$Version,
    [switch]$DryRun,
    [switch]$Prerelease,
    [string]$NotesFile,
    # Updater signing key (D-041). TAURI_SIGNING_PRIVATE_KEY in the environment wins.
    [string]$KeyPath = (Join-Path $env:USERPROFILE ".better-nectar\updater.key"),
    # Test builds only: skip the clean-tree and version-gate checks.
    [switch]$AllowDirty
)

# Builds a Better Nectar release from a committed, exported tree into release/. Never pushes and
# never touches GitHub: it prints the gh commands for the orchestrator to run after approval.

$ErrorActionPreference = "Stop"

function Invoke-Step([string]$label, [scriptblock]$body) {
    Write-Host "`n==> $label" -ForegroundColor Cyan
    $ErrorActionPreference = "Continue"
    & $body
    $code = $LASTEXITCODE
    $ErrorActionPreference = "Stop"
    if ($code -ne 0) { throw "$label failed (exit code $code)" }
}

function Get-SemVer([string]$v) { [version]($v.TrimStart("v")) }

$repo = "vekilberke/better-nectar"
$tag = "v$Version"
$root = Resolve-Path "$PSScriptRoot/.."
Set-Location $root

if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 2.0.0" }

$conf = Get-Content src-tauri/tauri.conf.json -Raw | ConvertFrom-Json
$product = $conf.productName
$binary = $conf.mainBinaryName
$assetBase = ($product.ToLower() -replace '[^a-z0-9]+', '-').Trim('-')

$versionFiles = @{
    "package.json"              = (Get-Content package.json -Raw | ConvertFrom-Json).version
    "src-tauri/tauri.conf.json" = $conf.version
    "src-tauri/Cargo.toml"      = [regex]::Match((Get-Content src-tauri/Cargo.toml -Raw), '(?m)^version = "([^"]*)"').Groups[1].Value
}
$mismatched = $versionFiles.GetEnumerator() | Where-Object { $_.Value -ne $Version }
if ($mismatched) {
    $mismatched | ForEach-Object { Write-Host "  $($_.Key) is $($_.Value)" -ForegroundColor Yellow }
    throw "Version files don't match $Version. Run 'bun run bump $Version' and commit before releasing."
}

# Friends only get an update when the version is higher than the last release.
$ErrorActionPreference = "Continue"
$remoteTags = git ls-remote --tags "https://github.com/$repo.git" 2>$null | ForEach-Object { ($_ -split "refs/tags/")[1] }
$ErrorActionPreference = "Stop"
if (-not $remoteTags) { $remoteTags = git tag --list "v*" }
$newest = $remoteTags | Where-Object { $_ -match '^v\d+\.\d+\.\d+$' } | Sort-Object { Get-SemVer $_ } | Select-Object -Last 1
if ($newest -and (Get-SemVer $Version) -le (Get-SemVer $newest)) {
    if ($AllowDirty) { Write-Host "Version $Version is not above $newest (allowed for a test build)" -ForegroundColor Yellow }
    else { throw "Version $Version must be higher than the newest release $newest." }
}

function Assert-Clean([string]$when) {
    $dirty = git status --porcelain
    if ($dirty) {
        if ($AllowDirty) { Write-Host "Working tree not clean $when (allowed for a test build)" -ForegroundColor Yellow }
        else { throw "Working tree not clean $when; the binary must match the tagged commit.`n$dirty" }
    }
}
Assert-Clean "before the build"

$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "src-tauri/target" }
$release = Join-Path $target "release"
$out = Join-Path $root "release"
if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory $out | Out-Null

if ($NotesFile) {
    $notes = [string](Get-Content $NotesFile -Raw)
} else {
    $notes = (git log -1 --pretty=format:"%b" --no-merges | Out-String).Trim()
    if (-not $notes) { $notes = (git log -1 --pretty=format:"%s" --no-merges | Out-String).Trim() }
    if ($newest) { $notes += "`n`n**Full Changelog**: https://github.com/$repo/compare/$newest...$tag" }
}

# The installer ships WebView2Loader.dll next to the exe (tauri.release.conf.json, I-321).
$loader = Get-ChildItem (Join-Path $env:USERPROFILE ".cargo/registry/src") -Recurse -Filter WebView2Loader.dll -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match 'webview2-com-sys-[\d.]+[\\/]x64[\\/]' } | Sort-Object FullName | Select-Object -Last 1
if (-not $loader) { throw "WebView2Loader.dll not found; run 'cargo fetch --locked' in src-tauri first." }
New-Item -ItemType Directory (Join-Path $root "src-tauri/runtime") -Force | Out-Null
Copy-Item $loader.FullName (Join-Path $root "src-tauri/runtime/WebView2Loader.dll") -Force

if (Test-Path env:RUSTFLAGS) { throw "RUSTFLAGS is set and would override +crt-static in src-tauri/.cargo/config.toml." }

Invoke-Step "Installing frontend dependencies" { bun install --frozen-lockfile }
Invoke-Step "Fetching crates" { Push-Location src-tauri; try { cargo fetch --locked } finally { Pop-Location } }
# No key in the environment while build scripts and package scripts run.
$savedKey = $env:TAURI_SIGNING_PRIVATE_KEY
$savedPassword = $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD
Remove-Item env:TAURI_SIGNING_PRIVATE_KEY, env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
Invoke-Step "Building $product (no bundle, no key)" { bun run tauri build --no-bundle '--' --locked }

$bundleLog = Join-Path $out "bundle.log"
try {
    if ($savedKey) { $env:TAURI_SIGNING_PRIVATE_KEY = $savedKey }
    elseif (Test-Path $KeyPath) { $env:TAURI_SIGNING_PRIVATE_KEY = [IO.File]::ReadAllText($KeyPath).Trim() }
    else { throw "No signing key: set TAURI_SIGNING_PRIVATE_KEY or pass -KeyPath." }
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = if ($savedPassword) { $savedPassword } else { "" }
    Invoke-Step "Bundling the NSIS installer (signed)" {
        bun run tauri bundle --bundles nsis --config src-tauri/tauri.release.conf.json 2>&1 | Tee-Object -FilePath $bundleLog
    }
} finally {
    Remove-Item env:TAURI_SIGNING_PRIVATE_KEY, env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
}
if (Select-String -Path $bundleLog -Pattern "does not match the public key" -Quiet) {
    throw "The signing key does not match plugins.updater.pubkey in tauri.conf.json."
}

$exe = Join-Path $release "$binary.exe"
$setup = Join-Path $release "bundle/nsis/${product}_${Version}_x64-setup.exe"
$nsi = Join-Path $release "nsis/x64/installer.nsi"
foreach ($f in @($exe, $setup, "$setup.sig", $nsi)) {
    if (-not (Test-Path $f)) { throw "Expected build output not found: $f" }
}

Write-Host "`n==> Checking the build" -ForegroundColor Cyan
$imports = llvm-objdump -p $exe | Select-String "DLL Name"
if ($imports -match "libunwind") { throw "$binary.exe imports libunwind.dll; +crt-static was not applied." }
if (-not (Select-String -Path $nsi -Pattern "WebView2Loader.dll" -Quiet)) { throw "installer.nsi does not ship WebView2Loader.dll." }
Invoke-Step "Leak check (phone link relay and PWA)" { node scripts/check-shared-build.mjs --dist dist --exe $exe }
Invoke-Step "Third-party licences" { node scripts/third-party-licenses.mjs --out (Join-Path $out "THIRD_PARTY_LICENSES.md") }
Remove-Item (Join-Path $root "src-tauri/runtime") -Recurse -Force
Assert-Clean "after the build"

# GitHub turns spaces in asset names into dots, so every asset gets a space-free name.
$setupName = "${assetBase}_${Version}_x64-setup.exe"
$updaterName = "${assetBase}_${Version}_x64-updater.exe"
Copy-Item $setup (Join-Path $out $setupName)
Copy-Item $setup (Join-Path $out $updaterName)
Copy-Item "$setup.sig" (Join-Path $out "$updaterName.sig")
$platform = [ordered]@{
    signature = (Get-Content "$setup.sig" -Raw).Trim()
    url       = "https://github.com/$repo/releases/download/$tag/$updaterName"
}
$manifest = [ordered]@{
    version   = $Version
    notes     = $notes
    pub_date  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    platforms = [ordered]@{
        "windows-x86_64"      = $platform
        "windows-x86_64-nsis" = $platform
    }
}
$utf8 = New-Object Text.UTF8Encoding $false
[IO.File]::WriteAllText((Join-Path $out "latest.json"), ($manifest | ConvertTo-Json -Depth 5), $utf8)
[IO.File]::WriteAllText((Join-Path $out "notes.md"), $notes, $utf8)
$sums = @($setupName, $updaterName) | ForEach-Object {
    "{0}  {1}" -f (Get-FileHash (Join-Path $out $_) -Algorithm SHA256).Hash.ToLower(), $_
}
[IO.File]::WriteAllText((Join-Path $out "SHA256SUMS.txt"), (($sums -join "`n") + "`n"), $utf8)
Remove-Item $bundleLog -ErrorAction SilentlyContinue

$assets = @($setupName, $updaterName, "$updaterName.sig", "latest.json", "SHA256SUMS.txt", "THIRD_PARTY_LICENSES.md")
Write-Host "`nBuilt in ${out}:" -ForegroundColor Green
$assets | ForEach-Object { Write-Host ("  {0}  ({1:N1} KB)" -f $_, ((Get-Item (Join-Path $out $_)).Length / 1KB)) }

$sha = (git rev-parse HEAD).Trim()
$latest = if ($Prerelease) { "--prerelease" } else { "--latest" }
$files = ($assets | ForEach-Object { "release/$_" }) -join " "
Write-Host "`nNothing was pushed or published$(if ($DryRun) { ' (dry run)' }). After the user's approval, from the export repo:" -ForegroundColor Yellow
Write-Host "  gh release create $tag --repo $repo --target $sha --title `"Better Nectar $tag`" --notes-file release/notes.md --draft $files"
Write-Host "  gh release edit $tag --repo $repo --draft=false $latest"

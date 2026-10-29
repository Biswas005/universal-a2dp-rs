#Requires -RunAsAdministrator
$ErrorActionPreference = "Stop"

$WorkspaceRoot = $PSScriptRoot
$PackageDir    = Join-Path $WorkspaceRoot "target\package"
$CertName      = "UniversalA2DP Test Root"

Write-Host "[1/6] Building Rust Kernel Driver (universal_a2dp.sys) & User Service..." -ForegroundColor Cyan
cargo build --release

# Verify the compiled artifacts exist
$SysSource = Join-Path $WorkspaceRoot "target\release\universal_a2dp.dll"
$InxSource = Join-Path $WorkspaceRoot "crates\kernel-driver\universal_a2dp.inx"

if (-not (Test-Path $SysSource)) {
    throw "Kernel binary not found at $SysSource. Check cargo build output."
}

# Prepare clean package staging folder
if (Test-Path $PackageDir) { Remove-Item $PackageDir -Recurse -Force }
New-Item -ItemType Directory -Path $PackageDir | Out-Null

# Copy .dll output from cargo cdylib as .sys kernel binary
$SysTarget = Join-Path $PackageDir "universal_a2dp.sys"
$InfTarget = Join-Path $PackageDir "universal_a2dp.inf"
Copy-Item $SysSource $SysTarget
Copy-Item $InxSource $InfTarget

Write-Host "[2/6] Locating Windows 11 WDK Packaging Tools (stampinf, Inf2Cat, signtool)..." -ForegroundColor Cyan
$WdkBinRoot = "C:\Program Files (x86)\Windows Kits\10\bin"
$LatestSdk  = Get-ChildItem $WdkBinRoot -Directory | Where-Object { $_.Name -match "^10\.0\." } | Sort-Object Name -Descending | Select-Object -First 1
$ToolDirX64 = Join-Path $LatestSdk.FullName "x64"
$ToolDirX86 = Join-Path $LatestSdk.FullName "x86"

$StampInf = Join-Path $ToolDirX86 "stampinf.exe"
$Inf2Cat  = Join-Path $ToolDirX86 "Inf2Cat.exe"
$SignTool = Join-Path $ToolDirX64 "signtool.exe"

Write-Host "[3/6] Running stampinf on universal_a2dp.inf..." -ForegroundColor Cyan
& $StampInf -f $InfTarget -d "*" -a "amd64" -c "universal_a2dp.cat" -v "*" -k "1.33"

Write-Host "[4/6] Generating Security Catalog (universal_a2dp.cat) via Inf2Cat..." -ForegroundColor Cyan
& $Inf2Cat /driver:$PackageDir /os:10_VB_X64 /verbose

Write-Host "[5/6] Creating / Locating Test Certificate & Signing Driver Package..." -ForegroundColor Cyan
$Cert = Get-ChildItem Cert:\LocalMachine\My | Where-Object { $_.Subject -eq "CN=$CertName" } | Select-Object -First 1

if (-not $Cert) {
    $Cert = New-SelfSignedCertificate `
        -Type CodeSigningCert `
        -Subject "CN=$CertName" `
        -KeyAlgorithm RSA `
        -KeyLength 2048 `
        -HashAlgorithm SHA256 `
        -CertStoreLocation "Cert:\LocalMachine\My"

    # Trust the test certificate in Root and TrustedPublisher stores
    $TempCer = Join-Path $PackageDir "testcert.cer"
    Export-Certificate -Cert $Cert -FilePath $TempCer | Out-Null
    Import-Certificate -FilePath $TempCer -CertStoreLocation "Cert:\LocalMachine\Root" | Out-Null
    Import-Certificate -FilePath $TempCer -CertStoreLocation "Cert:\LocalMachine\TrustedPublisher" | Out-Null
    Remove-Item $TempCer -Force
}

$CatTarget = Join-Path $PackageDir "universal_a2dp.cat"
& $SignTool sign /v /fd SHA256 /s My /n $CertName $SysTarget
& $SignTool sign /v /fd SHA256 /s My /n $CertName $CatTarget

Write-Host "[6/6] Adding Signed Driver to Windows 11 Driver Store via pnputil..." -ForegroundColor Cyan
pnputil /add-driver $InfTarget /install

Write-Host "`nDone! Driver staged in Windows 11 Driver Store: $PackageDir" -ForegroundColor Green
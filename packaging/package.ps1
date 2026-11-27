# Build the release exe and produce the portable zip (+ the Inno Setup
# installer when ISCC.exe is available).
# Usage: powershell -ExecutionPolicy Bypass -File packaging\package.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

    $ver = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages[0].version
    $name = "zonelock-$ver-win-x64"
    $dist = Join-Path $root "dist\$name"
    if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
    New-Item -ItemType Directory -Force -Path $dist | Out-Null

    Copy-Item target\release\zonelock.exe $dist\
    Copy-Item README.md, config.example.json $dist\
    if (Test-Path LICENSE) { Copy-Item LICENSE $dist\ }

    $zip = "$dist.zip"
    if (Test-Path $zip) { Remove-Item $zip }
    Compress-Archive -Path "$dist\*" -DestinationPath $zip

    $exe = Get-Item "$dist\zonelock.exe"
    Write-Host ("`nPortable: {0}  ({1:N0} KB exe)" -f $zip, ($exe.Length / 1KB))

    # optional installer
    $iscc = @(
        (Get-Command iscc -ErrorAction SilentlyContinue).Source,
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    # raw exe: what a portable copy downloads to update itself
    $rawExe = Join-Path $root "dist\$name.exe"
    Copy-Item target\release\zonelock.exe $rawExe
    $artifacts = @($zip, $rawExe)
    if ($iscc) {
        & $iscc /Q "/DAppVersion=$ver" packaging\ZoneLock.iss
        if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
        $setup = Join-Path $root "dist\zonelock-$ver-setup.exe"
        Write-Host "Installer: $setup"
        $artifacts += $setup
    } else {
        Write-Host "Installer skipped (Inno Setup 6 not found)."
    }

    # optional MSI (WiX Toolset 5: dotnet tool install --global wix)
    if (Get-Command wix -ErrorAction SilentlyContinue) {
        $msi = Join-Path $root "dist\zonelock-$ver-win-x64.msi"
        wix extension add -g WixToolset.Util.wixext/5.0.2 | Out-Null
        $ico = Get-ChildItem target\release\build\zonelock-*\out\zonelock.ico -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTime -Descending | Select-Object -First 1
        $icoArg = if ($ico) { @('-d', "IconFile=$($ico.FullName)") } else { @() }
        wix build packaging\ZoneLock.wxs -arch x64 -d "AppVersion=$ver" @icoArg -ext WixToolset.Util.wixext -o $msi
        if ($LASTEXITCODE -ne 0) { throw "wix build failed" }
        Remove-Item (Join-Path $root "dist\zonelock-$ver-win-x64.wixpdb") -ErrorAction SilentlyContinue
        Write-Host "MSI: $msi"
        $artifacts += $msi
    } else {
        Write-Host "MSI skipped (WiX Toolset not found: dotnet tool install --global wix --version 5.0.2)."
    }

    # checksums next to the artifacts
    $sums = $artifacts | ForEach-Object {
        "{0}  {1}" -f (Get-FileHash $_ -Algorithm SHA256).Hash.ToLower(), (Split-Path $_ -Leaf)
    }
    $sums | Set-Content -Encoding ascii (Join-Path $root "dist\SHA256SUMS.txt")
    Write-Host "Checksums: dist\SHA256SUMS.txt"
}
finally {
    Pop-Location
}

# 本地打包脚本：按指定身份构建，放入 usbip-win2 安装程序和许可声明，调用 vpk 生成安装版和便携版。
# `vpk upload` 和打 tag 由维护者手动执行。用 pwsh 运行：
#   pwsh scripts/release.ps1 [-Identity production|development]
# 版本号取自同目录的 version.json，程序里显示的版本也来自它（构建时读取），两边不会对不上。
[CmdletBinding()]
param(
    [ValidateSet('production', 'development')][string]$Identity = 'production'
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$Root = Split-Path $PSScriptRoot -Parent
$Version = (Get-Content (Join-Path $PSScriptRoot 'version.json') -Raw | ConvertFrom-Json).version
$Stage = Join-Path $Root "target\release-package\$Identity\$Version"
$Publish = Join-Path $Stage 'publish'
$Output = Join-Path $Stage 'artifacts'
$VpkDir = Join-Path $Root 'agent_tmp\tmp\vpk'
$VpkVersion = '1.2.161'
$UsbipVersion = '0.9.8.1'
$UsbipInstaller = "USBip-$UsbipVersion-x64.exe"
$UsbipUrl = "https://github.com/vadimgrn/usbip-win2/releases/download/v.$UsbipVersion/$UsbipInstaller"
$UsbipSha256 = '38cad6d4432b52d5bb9409d9ad03b72fdffc4ada4cd3a48fbeca1a2752a8518a'
$PackId = if ($Identity -eq 'production') { 'Aemeath' } else { 'AemeathDev' }
$PackTitle = if ($Identity -eq 'production') { 'Aemeath' } else { 'Aemeath Dev' }
$FeatureFlags = if ($Identity -eq 'production') { @('--features', 'production-identity') } else { @() }

function Invoke-Step([string]$Title, [scriptblock]$Action) {
    Write-Host "==> $Title"
    & $Action
}

Remove-Item $Stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force (Join-Path $Publish 'redist'), (Join-Path $Publish 'licenses'), $Output | Out-Null

Push-Location $Root
try {
    Invoke-Step 'Build the UI and the app' {
        Push-Location app
        try {
            # tauri build 负责嵌入前端资源；不生成 Tauri 自己的安装包
            pnpm tauri build --no-bundle @FeatureFlags
        }
        finally {
            Pop-Location
        }
    }
    Invoke-Step 'Build the engine and the guardian' {
        cargo build --release -p aemeath-host @FeatureFlags
        cargo build --release -p aemeath-guardian @FeatureFlags
    }
    Invoke-Step 'Collect the executables' {
        foreach ($exe in 'Aemeath.exe', 'aemeath-host.exe', 'aemeath-app.exe') {
            Copy-Item (Join-Path 'target\release' $exe) $Publish
        }
    }
    Invoke-Step 'Download and verify the usbip-win2 installer' {
        $installer = Join-Path $Publish "redist\$UsbipInstaller"
        Invoke-WebRequest -Uri $UsbipUrl -OutFile $installer
        $actual = (Get-FileHash $installer -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $UsbipSha256) {
            throw "SHA256 mismatch for ${UsbipInstaller}: $actual"
        }
    }
    Invoke-Step 'Write the third-party notices' {
        Copy-Item LICENSE (Join-Path $Publish 'licenses\Aemeath-AGPL-3.0.txt')
        $bsd = "usbip-win2 $UsbipVersion is distributed under the BSD 2-Clause license.`nSource and license text: https://github.com/vadimgrn/usbip-win2`n"
        Set-Content -Path (Join-Path $Publish 'licenses\usbip-win2.txt') -Value $bsd -NoNewline -Encoding utf8NoBOM
        $crates = cargo tree --workspace -e normal --prefix none --format '{p} ({l})' |
            ForEach-Object { $_ -replace ' \(\*\)$', '' } |
            Where-Object { $_ -notmatch '^aemeath' -and $_ -ne '' } |
            Sort-Object -Unique
        $crates | Set-Content -Path (Join-Path $Publish 'licenses\rust-dependencies.txt') -Encoding utf8NoBOM
    }
    Invoke-Step 'Install vpk into the temporary tools folder' {
        $vpk = Join-Path $VpkDir 'vpk.exe'
        if (-not (Test-Path $vpk)) {
            dotnet tool install --tool-path $VpkDir vpk --version $VpkVersion
        }
    }
    Invoke-Step 'Package' {
        # vpk 针对较早的 .NET 构建，本机只有更新的 SDK 时要允许跨主版本运行
        $env:DOTNET_ROLL_FORWARD = 'Major'
        & (Join-Path $VpkDir 'vpk.exe') pack `
            --packId $PackId `
            --packVersion $Version `
            --packTitle $PackTitle `
            --packDir $Publish `
            --runtime win-x64 `
            --mainExe Aemeath.exe `
            --icon app\src-tauri\icons\icon.ico `
            --delta none `
            --outputDir $Output
    }
}
finally {
    Pop-Location
}

Write-Host "Artifacts: $Output"
Get-ChildItem $Output | ForEach-Object { Write-Host "  $($_.Name)" }

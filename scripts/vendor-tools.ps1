# Fournit l'app en outils portables (qpdf, ffmpeg, adb) sans que
# l'utilisateur final ait besoin de les installer séparément, et facilite
# l'installation de LibreOffice (nécessaire pour convertir les anciens
# .ppt/.pps).
#
# Ce script télécharge :
#   - qpdf (portable, licence Apache-2.0)      -> pour l'Optimizer PDF sans perte
#   - ffmpeg build LGPL (BtbN/FFmpeg-Builds)    -> pour l'Optimizer vidéo (remux "-c copy")
#   - adb (Android Platform Tools, Apache-2.0,
#     distribution officielle Google)           -> pour TakePhotoOnPhone
# et les place dans `src-tauri/tools/`, là où `resolve_tool()` (core/src/fs_utils.rs)
# les cherche en premier, avant de retomber sur le PATH système. resolve_tool()
# accepte plusieurs arrangements (tools/<nom>.exe à plat, tools/<nom>/bin/<nom>.exe,
# tools/<nom>/<nom>.exe) — qpdf utilise ici tools/qpdf/bin/qpdf.exe pour garder
# tout son dossier bin/ (et donc ses DLL) intact tel que livré par le zip officiel,
# ce qui évite les mismatches de DLL (cause probable des erreurs
# STATUS_ENTRYPOINT_NOT_FOUND / 0xc0000139 rencontrées avec un simple copier-coller
# de qpdf.exe isolé).
#
# En plus : télécharge l'installeur officiel de LibreOffice dans un dossier
# `downloads/` séparé (PAS dans src-tauri/tools/ — ce dossier est embarqué
# dans l'app livrée via bundle.resources, et un installeur de ~300 Mo n'a
# rien à y faire). LibreOffice est différent des trois outils ci-dessus :
# c'est une vraie suite bureautique qui a besoin d'une installation
# complète (registre, raccourcis...), pas d'une copie portable qu'on peut
# juste déposer dans un dossier — donc ce script télécharge l'installeur
# pour toi, mais tu dois double-cliquer dessus toi-même une fois.
#
# Pourquoi pas Ghostscript (compression PDF *avec perte*) ? Sa licence AGPL
# impose des obligations de redistribution qui ne sont pas anodines pour une
# app qu'on distribue à des tiers. Le mode PDF avec perte reste disponible,
# mais seulement si l'utilisateur installe `gs` lui-même.
#
# Pourquoi la variante LGPL de ffmpeg et pas la "full/gpl" (avec libx264) ?
# Même raison : GPL est plus contraignant à redistribuer que LGPL. Le remux
# sans perte ("-c copy", le mode par défaut de l'Optimizer vidéo) fonctionne
# très bien avec le build LGPL.
#
# Usage :  powershell -ExecutionPolicy Bypass -File scripts\vendor-tools.ps1
#          (ajoute -SkipLibreOffice pour ne pas telecharger l'installeur, ex. en CI)

param([switch]$SkipLibreOffice)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$toolsDir = Join-Path $root "src-tauri\tools"
New-Item -ItemType Directory -Force -Path $toolsDir | Out-Null
# A FRESH temp dir every run, wiped first if it already exists. This matters:
# without wiping, a stale extracted folder from an earlier run (e.g. an old
# "qpdf-*-mingw64" folder from before this script switched to msvc64) could
# still be sitting here and get silently picked up instead of the build we
# just downloaded — Get-ChildItem's default alphabetical order even favors
# "mingw64" over "msvc64", so this could go unnoticed. Separate subfolders
# per tool below add a second layer of protection against the same mistake.
$tmp = Join-Path $env:TEMP "ftp-vendor-tools"
if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

function Get-LatestReleaseAsset($repo, $namePatterns) {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" -Headers @{ "User-Agent" = "file-toolkit-pro" }
    foreach ($pattern in $namePatterns) {
        $asset = $release.assets | Where-Object { $_.name -match $pattern } | Select-Object -First 1
        if ($asset) { return $asset }
    }
    throw "Aucun asset correspondant a l'un de '$($namePatterns -join ', ')' trouve pour $repo"
}

Write-Host "== qpdf =="
# msvc64 first: it only depends on the universal Visual C++ runtime that
# ships with Windows 10/11 itself. The mingw64 build (tried second, as a
# fallback) additionally depends on the MinGW/MSYS2 runtime, which is often
# what's actually missing on a "clean" Windows machine and the likely real
# cause of STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139) even with every DLL from
# the release zip copied alongside qpdf.exe.
$qpdfAsset = Get-LatestReleaseAsset "qpdf/qpdf" @("msvc64\.zip$", "mingw64\.zip$")
$qpdfZip = Join-Path $tmp $qpdfAsset.name
Invoke-WebRequest -Uri $qpdfAsset.browser_download_url -OutFile $qpdfZip
# Extracted into its OWN subfolder (not shared with ffmpeg/adb) so a
# same-named file/folder from another tool can never be picked up by mistake.
$qpdfExtract = Join-Path $tmp "qpdf-extract"
Expand-Archive -Path $qpdfZip -DestinationPath $qpdfExtract -Force

$qpdfRoot = Get-ChildItem -Path $qpdfExtract -Directory -Filter "qpdf-*" | Select-Object -First 1
if (-not $qpdfRoot) {
    $qpdfRoot = Get-ChildItem -Path $qpdfExtract -Recurse -Filter "qpdf.exe" |
        ForEach-Object { $_.Directory.Parent } |
        Select-Object -First 1
}
if (-not $qpdfRoot) { throw "Impossible de trouver la racine de qpdf dans le ZIP" }

$qpdfDest = Join-Path $toolsDir "qpdf"
if (Test-Path $qpdfDest) { Remove-Item $qpdfDest -Recurse -Force }
Copy-Item -Path $qpdfRoot.FullName -Destination $qpdfDest -Recurse -Force
Write-Host "qpdf.exe pret dans $qpdfDest\bin"

Write-Host "== ffmpeg (LGPL) =="
$ffAsset = Get-LatestReleaseAsset "BtbN/FFmpeg-Builds" @("ffmpeg-master-latest-win64-lgpl\.zip$")
$ffZip = Join-Path $tmp $ffAsset.name
Invoke-WebRequest -Uri $ffAsset.browser_download_url -OutFile $ffZip
$ffExtract = Join-Path $tmp "ffmpeg-extract"
Expand-Archive -Path $ffZip -DestinationPath $ffExtract -Force
$ffExe = Get-ChildItem -Path $ffExtract -Recurse -Filter "ffmpeg.exe" | Select-Object -First 1
Copy-Item $ffExe.FullName -Destination (Join-Path $toolsDir "ffmpeg.exe") -Force
Get-ChildItem -Path $ffExe.DirectoryName -Filter "*.dll" -ErrorAction SilentlyContinue | ForEach-Object {
    Copy-Item $_.FullName -Destination $toolsDir -Force
}
Write-Host "ffmpeg.exe pret dans $toolsDir"

Write-Host "== adb (Android Platform Tools, Apache-2.0, distribution officielle Google) =="
$adbZip = Join-Path $tmp "platform-tools-latest-windows.zip"
Invoke-WebRequest -Uri "https://dl.google.com/android/repository/platform-tools-latest-windows.zip" -OutFile $adbZip
$adbExtractRoot = Join-Path $tmp "adb-extract"
Expand-Archive -Path $adbZip -DestinationPath $adbExtractRoot -Force
$adbExtract = Join-Path $adbExtractRoot "platform-tools"
Copy-Item (Join-Path $adbExtract "adb.exe") -Destination (Join-Path $toolsDir "adb.exe") -Force
Get-ChildItem -Path $adbExtract -Filter "*.dll" | ForEach-Object {
    Copy-Item $_.FullName -Destination $toolsDir -Force
}
Write-Host "adb.exe pret dans $toolsDir"

if (-not $SkipLibreOffice) {
    Write-Host "== LibreOffice (pour convertir les anciens .ppt/.pps en .pptx) =="
    # LibreOffice is NOT like qpdf/ffmpeg/adb above: it's a full office suite
    # (~300 Mo), not a small portable utility, and the official build always
    # needs a real installation (registry, Start Menu, file associations) — it
    # can't just be dropped in tools/ and picked up silently. So this step only
    # downloads the OFFICIAL installer for convenience; YOU run it once
    # yourself. Once installed anywhere on the system, resolve_tool("soffice")
    # finds it automatically (PATH, or the default Windows install location).
    #
    # There's no simple stable "always latest" download URL like qpdf/ffmpeg's
    # GitHub releases, so this scrapes the current version's direct download
    # link off LibreOffice's own download page. That page's structure could
    # change and break this step — if it does, the rest of the script (qpdf,
    # ffmpeg, adb, all of which DO work fully automatically) is unaffected;
    # you'll just get a manual-download link printed instead.
    try {
        $dlPage = Invoke-WebRequest -Uri "https://www.libreoffice.org/download/download-libreoffice/?type=win-x86_64" -UseBasicParsing
        $match = [regex]::Match(
            $dlPage.Content,
            'https://download\.documentfoundation\.org/libreoffice/stable/[\d.]+/win/x86_64/LibreOffice_[\d.]+_Win_x86-64\.msi'
        )
        if (-not $match.Success) { throw "lien de telechargement introuvable sur la page LibreOffice (structure de page changee ?)" }

        $downloadsDir = Join-Path $root "downloads"
        New-Item -ItemType Directory -Force -Path $downloadsDir | Out-Null
        $loMsi = Join-Path $downloadsDir "LibreOffice_Installer.msi"
        Invoke-WebRequest -Uri $match.Value -OutFile $loMsi
        Write-Host "  OK  Installeur telecharge : $loMsi (~300 Mo)."
        Write-Host "      Double-clique dessus pour l'installer (une seule fois, quelques minutes)."
        Write-Host "      Une fois installe, l'app le trouve automatiquement — rien d'autre a faire."
    } catch {
        Write-Host "  Telechargement automatique impossible : $($_.Exception.Message)"
        Write-Host "  Telecharge-le toi-meme (gratuit) : https://www.libreoffice.org/download/download-libreoffice/"
    }
}

Write-Host ""
Write-Host "Termine. $toolsDir contient maintenant qpdf/bin/qpdf.exe, ffmpeg.exe et adb.exe."
Write-Host "cargo tauri build copiera ce dossier a cote de l'executable (voir bundle.resources"
Write-Host "dans tauri.conf.json). Pour 'cargo tauri dev' / un simple 'cargo build', copie"
Write-Host "aussi src-tauri\tools vers target\debug\tools ou target\release\tools."

Write-Host ""
Write-Host "== Verification =="

function Test-Tool($exePath, $label, $versionArg) {
    try {
        & $exePath $versionArg *> $null
        if ($LASTEXITCODE -eq 0) {
            Write-Host "  OK  $label repond correctement."
        } else {
            Write-Host "  ECHEC  $label a demarre mais renvoie le code $LASTEXITCODE."
            Write-Host "         Solution de secours : installe-le normalement (ex. 'winget install --id QPDF.QPDF -e')"
            Write-Host "         et laisse resolve_tool() retomber sur le PATH."
        }
    } catch {
        Write-Host "  ECHEC  $label n'a pas pu demarrer ($($_.Exception.Message))."
        Write-Host "         Solution de secours : installe-le normalement (ex. 'winget install --id QPDF.QPDF -e')"
        Write-Host "         et laisse resolve_tool() retomber sur le PATH."
    }
}

Test-Tool (Join-Path $toolsDir "qpdf\bin\qpdf.exe") "qpdf.exe" "--version"
Test-Tool (Join-Path $toolsDir "ffmpeg.exe") "ffmpeg.exe" "-version"
Test-Tool (Join-Path $toolsDir "adb.exe") "adb.exe" "version"

$sofficeCandidates = @(
    "C:\Program Files\LibreOffice\program\soffice.exe",
    "C:\Program Files (x86)\LibreOffice\program\soffice.exe"
)
$sofficeFound = $sofficeCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if ($sofficeFound) {
    Test-Tool $sofficeFound "soffice.exe" "--version"
} elseif (-not $SkipLibreOffice) {
    Write-Host "  INFO  LibreOffice pas encore installe — normal si tu viens de telecharger l'installeur ci-dessus."
}

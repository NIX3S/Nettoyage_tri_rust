<div align="center">

<img src="src-tauri/icons/icon.png" alt="File Toolkit Pro" width="96" />

# File Toolkit Pro

**Six outils de gestion de fichiers dans une seule application de bureau, légère et sûre.**

[![CI](https://github.com/NIX3S/Nettoyage_tri_rust/actions/workflows/ci.yml/badge.svg)](https://github.com/OWNER/REPO/actions/workflows/ci.yml)
[![Release](https://github.com/NIX3S/Nettoyage_tri_rust/actions/workflows/release.yml/badge.svg)](https://github.com/OWNER/REPO/actions/workflows/release.yml)
![Platforms](https://img.shields.io/badge/Windows%20%7C%20macOS%20%7C%20Linux-informational)
![Rust](https://img.shields.io/badge/Rust-Tauri%20v2-orange)

</div>

File Toolkit Pro regroupe derrière une interface en *glassmorphism* des utilitaires qui existaient sous forme de scripts Python. Une seule application (un exécutable), construite en **Rust + Tauri v2**, pour Windows, macOS et Linux.

## Sommaire

- [Principes de sécurité](#principes-de-sécurité)
- [Fonctionnalités](#fonctionnalités)
- [Installation](#installation)
- [Outils externes](#outils-externes)
- [Compiler depuis les sources](#compiler-depuis-les-sources)
- [Intégration continue et releases](#intégration-continue-et-releases)
- [Configuration](#configuration)
- [Dépannage](#dépannage)
- [Architecture](#architecture)
- [Tests](#tests)
- [Pistes d'amélioration](#pistes-damélioration)

## Principes de sécurité

- **Aucune perte de données par défaut** : les suppressions passent par la **corbeille** du système, jamais de suppression définitive directe.
- **Confirmation** avant toute suppression réelle.
- **Originaux intacts**, sauf si tu choisis explicitement le mode « sur place » (avec `.bak` optionnel).
- **Une erreur sur un fichier n'interrompt jamais le lot** : elle est listée à la fin, le reste continue.
- **Pensé pour des utilisateurs non techniques** : aucune fenêtre console qui clignote, messages d'erreur clairs en français, progression en temps réel.

## Fonctionnalités

### 🔀 Comparaison
Compare deux arborescences A et B **par chemin relatif** (pas par contenu : voir Find Duplicate). Pour chaque fichier présent d'un seul côté : ouvrir l'emplacement, retirer de la vue (sans toucher au disque) ou **Auto update** (copie ou coupe vers l'autre arborescence en conservant la structure). Sélection multiple et actions par lot. Des [règles conditionnelles](#règles-dauto-update) permettent de couper certains chemins et de copier le reste.

### 🧬 Find Duplicate
Détection par **contenu réel** (taille identique, puis SHA-256), pas par nom. Les doublons sont regroupés ; tu choisis quel fichier garder par groupe, les autres partent à la **corbeille** après confirmation. Filtres : taille minimale, extensions, ancienneté maximale. Un groupe peut être masqué sans être traité.

### 🗜️ Optimizer
Source : dossier ou fichier unique. Destination : un autre dossier, ou **réécriture sur place** (`.bak` en option). **Sans perte par défaut.**

| Type | Traitement |
|---|---|
| Image | Ré-encodage PNG compression maximale (transparence) ou JPEG qualité 95 (photos), en Rust pur |
| PPTX et variantes OOXML (`.pptx` `.ppsx` `.pptm` `.ppsm` `.potx` `.potm`) | Archive reconstruite : images de `ppt/media` ré-encodées (gardées telles quelles si le résultat n'est pas plus petit), le reste copié à l'identique en Deflate maximal. Texte et structure jamais touchés |
| Anciens `.ppt` `.pps` `.pot` | Convertis en `.pptx` via LibreOffice (`soffice --headless`) puis optimisés |
| `.eml` | Compression sans perte en `.eml.zip` (le contenu n'est jamais réécrit) |
| Vidéo | `ffmpeg -c copy` (remux, zéro perte) |
| PDF | `qpdf --compress-streams` |

- Un résultat **plus gros** que l'original est ignoré : l'original est conservé et listé comme « inchangé ».
- En mode sur place, l'original d'un `.eml` ou `.ppt/.pps` converti est retiré une fois le nouveau fichier produit (renommé `.bak` si l'option est cochée, sinon envoyé à la corbeille). Un bouton permet de **nettoyer les `.bak`** après validation.
- Les fichiers de verrouillage Office (`~$...`) sont ignorés.
- **Mode avec perte** (menu avancé) : un curseur qualité 0–100 pilote la qualité JPEG, le CRF vidéo et le preset PDF (`screen` / `ebook` / `printer`, via Ghostscript, à installer toi-même).

### 📦 GetALL
Un ou plusieurs dossiers source vers un dossier de sortie unique, à plat. Filtre par catégorie (Image / Vidéo / Audio / Document) et/ou extensions. Copy ou Cut. Option d'export `getall_index.json` associant chaque fichier à son chemin d'origine.

### 🗂️ TriPhotosVideo
Classe photos et vidéos en `YYYY/MM` selon la date EXIF (photos), sinon la date de création du fichier ; sans date exploitable, dans un dossier `NSP`. Dédoublonnage SHA-256 optionnel, préfixe de dossier personnalisable (ex. `Trip_2025`). Destination : un dossier de base (avec séparation Photos/Videos optionnelle) ou **deux destinations indépendantes** (ex. photos sur `E:\`, vidéos sur `F:\`).

### 📱 TakePhotoOnPhone
Récupère des fichiers d'un téléphone Android via **ADB** : détection des appareils (avec état `unauthorized` / `offline` expliqué), Copy ou Cut. Les chemins relatifs (`DCIM/Camera`) sont complétés en `/storage/emulated/0/...`.

**Organisation des fichiers récupérés**

- **Arborescence complète** : chaque dossier demandé tel quel, sous-dossiers inclus (le plus rapide).
- **Seulement les dossiers demandés** : uniquement les fichiers directement dans le dossier, sans ses sous-dossiers.
- **Tout dans un seul dossier** : tous les fichiers, sous-dossiers inclus, à plat dans une seule destination.

**Règle de coupe** (mode Cut uniquement) : *Tout* (exactement ce qui a été récupéré), *À partir du / Avant le* une date, ou *Plus vieux / moins vieux que N jours* (ex. libérer les photos de plus d'un an). **Dédoublonnage SHA-256** optionnel après le transfert. La liste des dossiers peut être chargée et enregistrée dans un [fichier texte](#liste-de-dossiers-android).

## Installation

### Utilisateurs

Télécharge l'installeur de ta plateforme dans l'onglet **[Releases](../../releases)** :

| Plateforme | Fichiers |
|---|---|
| Windows | `.msi` ou `-setup.exe` |
| macOS (Apple Silicon et Intel) | `.dmg` |
| Linux | `.AppImage`, `.deb`, `.rpm` |

> Les builds ne sont pas signés : Windows SmartScreen et macOS Gatekeeper peuvent afficher un avertissement au premier lancement (sur macOS : clic droit, *Ouvrir*).

Pour **TakePhotoOnPhone**, active sur le téléphone le *débogage USB*, passe en mode *transfert de fichiers* et autorise l'ordinateur au premier branchement.

## Outils externes

| Outil | Sert à | Licence |
|---|---|---|
| `qpdf` | Optimizer PDF | Apache-2.0 |
| `ffmpeg` (build LGPL) | Optimizer vidéo | LGPL |
| `adb` | TakePhotoOnPhone | Apache-2.0 |
| `soffice` (LibreOffice) | Conversion `.ppt` / `.pps` | MPL-2.0 |
| `gs` (Ghostscript) | PDF avec perte (optionnel) | AGPL, jamais embarqué |

Ordre de recherche : dossier `tools/` à côté de l'exécutable, puis `PATH` système. Si un outil manque, l'erreur est claire et limitée au fichier concerné.

**Windows : embarqué dans l'installeur.** Lance une fois avant de compiler :

```powershell
powershell -ExecutionPolicy Bypass -File scripts\vendor-tools.ps1
```

Le script télécharge qpdf, ffmpeg (LGPL) et adb dans `src-tauri/tools/`, vérifie qu'ils démarrent et télécharge l'installeur de LibreOffice dans `downloads/` (à lancer toi-même, c'est une suite bureautique complète). Option `-SkipLibreOffice` pour l'ignorer (utilisé en CI).

**macOS / Linux** : les outils ne sont pas embarqués, installe-les via ton gestionnaire de paquets :

```bash
# macOS
brew install qpdf ffmpeg android-platform-tools
brew install --cask libreoffice
# Debian / Ubuntu
sudo apt install qpdf ffmpeg adb libreoffice-impress
```

## Compiler depuis les sources

**Prérequis** : [Rust](https://rustup.rs) stable, [Node.js](https://nodejs.org) 20+ (pour la CLI Tauri), et les dépendances système de Tauri v2.

```bash
git clone https://github.com/OWNER/REPO.git
cd REPO
npm install
npm run tauri dev       # développement
npm run tauri build     # installeurs dans target/release/bundle/
```

Sans Node : `cargo install tauri-cli --version "^2"` puis `cargo tauri dev` / `cargo tauri build`.

**Linux (Debian / Ubuntu)** :

```bash
sudo apt install build-essential libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev patchelf libxdo-dev libssl-dev
```

**macOS** : Xcode Command Line Tools (`xcode-select --install`). **Windows** : Visual Studio Build Tools (charge de travail C++) ; WebView2 est déjà présent sur Windows 10/11.

### Icônes

Les icônes sont dans `src-tauri/icons/` (PNG, `icon.ico` pour Windows, `icon.icns` pour macOS). Pour les régénérer à partir de ton propre visuel carré (1024 × 1024 recommandé) :

```bash
npm run tauri icon chemin/vers/mon-icone.png
```

## Intégration continue et releases

Deux workflows dans `.github/workflows/` :

- **`ci.yml`** : à chaque push sur `main` et chaque pull request, lance `cargo test -p ftp-core` sur Linux, Windows et macOS.
- **`release.yml`** : construit les installeurs pour **Windows x64, macOS Apple Silicon, macOS Intel et Linux x64** avec [`tauri-action`](https://github.com/tauri-apps/tauri-action) et les attache à une **release en brouillon**. Sous Windows, il exécute `vendor-tools.ps1 -SkipLibreOffice`, donc qpdf, ffmpeg et adb sont embarqués.

Publier une version :

```bash
# 1. mettre à jour "version" dans src-tauri/tauri.conf.json (et Cargo.toml)
git tag v0.1.0
git push origin v0.1.0
```

Les builds apparaissent dans **Actions** ; quand ils sont terminés, ouvre la release en brouillon dans **Releases** et publie-la. Le workflow se lance aussi à la main (*Run workflow*). Si le dépôt est privé, vérifie que **Settings > Actions > General > Workflow permissions** autorise *Read and write*.

## Configuration

### Règles d'Auto update

Dans l'onglet Comparaison (menu avancé) :

```json
{
  "rules": [
    { "path_contains": "Admin/Entreprise", "mode": "cut" }
  ],
  "default_mode": "copy"
}
```

Tout fichier dont le chemin relatif contient `Admin/Entreprise` est **coupé** vers l'autre dossier ; tout le reste est **copié**.

### Liste de dossiers Android

Un dossier par ligne, `#` pour commenter. Les chemins relatifs sont préfixés par `/storage/emulated/0/`. Modèle : [`config/android_folders.example.txt`](config/android_folders.example.txt).

```text
# Photos et vidéos
DCIM/Camera
Download
Android/media/com.whatsapp/WhatsApp/Media/WhatsApp Images
```

## Dépannage

| Symptôme | Cause et solution |
|---|---|
| « Aucun appareil détecté » | Téléphone en mode *transfert de fichiers*, débogage USB activé, et autorisation acceptée sur l'écran du téléphone (état `unauthorized`) |
| Erreur ADB « No such file or directory » | Le dossier n'existe pas sur ce téléphone (ex. les captures sont parfois dans `DCIM/Screenshots`, pas `Pictures/Screenshots`) : corrige la ligne |
| `qpdf` code `0xc0000139` (Windows) | DLL manquante ou version orpheline : relance `vendor-tools.ps1` (le build recopie le dossier `tools/` à neuf) ou installe qpdf avec `winget install QPDF.QPDF` |
| `.ppt` / `.pps` : « soffice introuvable » | Installe LibreOffice (voir [Outils externes](#outils-externes)) |
| « Invalid PNG signature » / « Could not find EOCD » | Le fichier est corrompu ou n'a pas le format de son extension ; il est listé en erreur et ignoré, le reste du lot continue |
| Boutons inactifs | Vérifie que `src-tauri/capabilities/default.json` contient `core:event:default`, puis recompile |
| Avertissement macOS / SmartScreen | Build non signé : clic droit > *Ouvrir* (macOS) ou *Informations complémentaires > Exécuter quand même* (Windows) |

## Architecture

```text
Cargo.toml              workspace Rust
core/                   ftp-core : logique pure, testée (comparison, duplicates, getall,
                        sort_media, optimizer, android, fs_utils)
src-tauri/              application Tauri v2 (commandes, build.rs, tauri.conf.json,
                        capabilities, icônes, tools/)
ui/                     interface (HTML / CSS / JS sans framework)
scripts/                vendor-tools.ps1 (outils embarqués Windows)
config/                 exemples de configuration
.github/workflows/      CI et releases
```

- Les commandes longues sont **asynchrones** (`spawn_blocking`), l'interface reste fluide.
- La progression passe par l'événement `ftp://progress` (`{tab, done, total, current}`), limité à ~120 ms.
- `AdbClient` (`core/src/android.rs`) est le seul code qui connaît ADB : un autre transport (MTP, app compagnon) peut le remplacer sans toucher au reste.
- **Pourquoi Tauri** : binaire léger, WebView native, un seul code pour trois OS, et l'effet glassmorphism se fait naturellement en CSS.

## Tests

```bash
cargo test -p ftp-core
```

Couvre la comparaison et l'auto-update, la détection de doublons, GetALL et le hachage. Lancés automatiquement par la CI sur les trois OS.

## Pistes d'amélioration

- JPEG réellement sans perte (`mozjpeg` ou `jpegtran`) : `image` ré-encode toujours.
- Progression plus fine pour Find Duplicate sur de très gros volumes.
- Journal JSON par exécution pour permettre un rollback global.
- Transparence réelle de la fenêtre via `window-vibrancy`.
- Signature de code (Windows, macOS) et notarisation dans le workflow de release.

## Licence

À définir. Note : ffmpeg (LGPL) et Ghostscript (AGPL) sont des composants tiers soumis à leurs propres licences ; Ghostscript n'est volontairement jamais embarqué.

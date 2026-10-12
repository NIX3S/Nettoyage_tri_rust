// File Toolkit Pro — frontend glue.
// Plain JS on purpose (no bundler/npm build step): withGlobalTauri exposes
// window.__TAURI__, so this file can be served as-is by the webview.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ---------------------------------------------------------------------
// Progress panel: real % + live "files processed" list, not just a status
// line. Shown for the duration of any long task, hidden a moment after.
// ---------------------------------------------------------------------
const progressPanel = document.getElementById("progressPanel");
const progressLabel = document.getElementById("progressLabel");
const progressPercent = document.getElementById("progressPercent");
const progressFill = document.getElementById("progressFill");
const progressFileList = document.getElementById("progressFileList");

const MAX_FILE_LIST_ENTRIES = 40;
let hidePanelTimer = null;

function showProgressPanel(label) {
  clearTimeout(hidePanelTimer);
  progressPanel.classList.add("active");
  progressLabel.textContent = label;
  progressPercent.textContent = "0 %";
  progressFill.style.width = "0%";
  progressFill.classList.remove("indeterminate");
  progressFileList.innerHTML = "";
}

function hideProgressPanelSoon() {
  hidePanelTimer = setTimeout(() => progressPanel.classList.remove("active"), 1400);
}

function pushFileListEntry(text) {
  const li = document.createElement("li");
  li.textContent = text;
  progressFileList.appendChild(li);
  while (progressFileList.children.length > MAX_FILE_LIST_ENTRIES) {
    progressFileList.removeChild(progressFileList.firstChild);
  }
}

let progressEventCount = 0;

// IMPORTANT: `listen()` is itself async — its promise only resolves once the
// event subscription is actually registered on the Rust side. We keep that
// promise and await it before running any task, so there's no window where
// an early progress event could be emitted before we're listening for it.
const progressListenerReady = listen("ftp://progress", (event) => {
  progressEventCount++;
  const { done, total, current } = event.payload;
  // Diagnostic: open DevTools (right-click the window > Inspecter, or
  // Ctrl+Shift+I) and watch the Console while a task runs. If these lines
  // stream in live during processing, the backend IS emitting progressively
  // and any remaining lag is a rendering issue. If they only appear once,
  // right at the very end, the backend isn't streaming — that's a different
  // bug than what's fixed so far.
  console.log(`[ftp progress #${progressEventCount}] ${done}/${total || "?"} ${current || ""}`);
  if (!total) {
    // Still counting / indeterminate phase (e.g. Find Duplicate's initial walk).
    progressFill.classList.add("indeterminate");
    progressPercent.textContent = `${done} fichier(s) trouvé(s)…`;
  } else {
    progressFill.classList.remove("indeterminate");
    const pct = Math.round((done / total) * 100);
    progressFill.style.width = `${pct}%`;
    progressPercent.textContent = `${pct} % (${done}/${total})`;
  }
  if (current) pushFileListEntry(current);
});

async function runTask(label, fn) {
  // A rejected progress-listener promise (e.g. missing permission) must
  // NEVER block the actual action — progress display is a nice-to-have,
  // the button's job is not. Swallow any listener-setup failure here so a
  // problem with the progress bar can't silently break every button.
  await progressListenerReady.catch((err) => {
    console.warn("Progress listener unavailable, continuing without live updates:", err);
  });
  progressEventCount = 0;
  showProgressPanel(`${label}…`);
  try {
    const result = await fn();
    progressLabel.textContent = `${label} — terminé (${progressEventCount} mise(s) à jour reçue(s)).`;
    progressFill.classList.remove("indeterminate");
    progressFill.style.width = "100%";
    hideProgressPanelSoon();
    return result;
  } catch (err) {
    progressLabel.textContent = `Erreur : ${err}`;
    progressFill.classList.remove("indeterminate");
    alert(`${label} a échoué :\n${err}`);
    throw err;
  }
}

function escapeHtml(str) {
  return String(str).replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}
function splitList(value) {
  return value.split(";").map((s) => s.trim()).filter(Boolean);
}
function humanSize(bytes) {
  const units = ["o", "Ko", "Mo", "Go", "To"];
  let size = bytes, i = 0;
  while (size >= 1024 && i < units.length - 1) { size /= 1024; i++; }
  return i === 0 ? `${bytes} o` : `${size.toFixed(1)} ${units[i]}`;
}

// ---------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------
document.getElementById("tabbar").addEventListener("click", (e) => {
  const btn = e.target.closest(".tab-btn");
  if (!btn) return;
  document.querySelectorAll(".tab-btn").forEach((b) => b.classList.remove("active"));
  document.querySelectorAll(".tab-panel").forEach((p) => p.classList.remove("active"));
  btn.classList.add("active");
  document.getElementById(`tab-${btn.dataset.tab}`).classList.add("active");
});

// ---------------------------------------------------------------------
// Native folder picker for every "Parcourir…" button
// ---------------------------------------------------------------------
document.body.addEventListener("click", async (e) => {
  const btn = e.target.closest(".browse-btn");
  if (!btn) return;
  const picked = await invoke("pick_folder_cmd");
  if (!picked) return;
  const input = document.getElementById(btn.dataset.target);
  input.value = input.value ? `${input.value}; ${picked}` : picked;
});

// =======================================================================
// COMPARAISON
// =======================================================================
let lastComparison = { only_in_a: [], only_in_b: [] };
let cmpRuleCount = 0;

document.getElementById("cmpAddRule").addEventListener("click", () => {
  const id = cmpRuleCount++;
  const wrap = document.createElement("div");
  wrap.className = "row";
  wrap.dataset.ruleRow = id;
  wrap.innerHTML = `
    <input type="text" placeholder="si le chemin contient…" class="rule-contains" style="flex:2;" />
    <select class="rule-mode"><option value="copy">Copy</option><option value="cut">Cut</option></select>
    <button class="btn btn-ghost btn-sm" data-remove-rule="${id}">✕</button>`;
  document.getElementById("cmpRulesList").appendChild(wrap);
});
document.getElementById("cmpRulesList").addEventListener("click", (e) => {
  const btn = e.target.closest("[data-remove-rule]");
  if (btn) btn.closest("[data-rule-row]").remove();
});

function readAutoUpdateConfig() {
  const rules = [...document.querySelectorAll("#cmpRulesList [data-rule-row]")].map((row) => ({
    path_contains: row.querySelector(".rule-contains").value,
    mode: row.querySelector(".rule-mode").value,
  })).filter((r) => r.path_contains);
  return { rules, default_mode: document.getElementById("cmpDefaultMode").value };
}

function renderComparisonTable(entries, side, destRoot) {
  if (!entries.length) return `<p class="empty-state">Rien à signaler.</p>`;
  const rows = entries.map((e, i) => `
    <tr data-idx="${i}" data-side="${side}">
      <td><input type="checkbox" class="cmp-select" /></td>
      <td class="path-cell">${escapeHtml(e.relative_path)}</td>
      <td>${humanSize(e.size)}</td>
      <td class="actions-cell">
        <button class="btn btn-secondary btn-sm cmp-reveal">Ouvrir l'emplacement</button>
        <button class="btn btn-secondary btn-sm cmp-remove">Retirer de l'export</button>
        <button class="btn btn-primary btn-sm cmp-update">Auto update</button>
      </td>
    </tr>`).join("");
  return `<table class="results" data-dest-root="${escapeHtml(destRoot)}">
    <thead><tr><th></th><th>Chemin relatif</th><th>Taille</th><th>Actions</th></tr></thead>
    <tbody>${rows}</tbody></table>`;
}

function renderComparisonResults() {
  const a = lastComparison.only_in_a, b = lastComparison.only_in_b;
  document.getElementById("cmpCountA").textContent = a.length;
  document.getElementById("cmpCountB").textContent = b.length;
  const bFolder = document.getElementById("cmpB").value;
  const aFolder = document.getElementById("cmpA").value;
  document.getElementById("cmpResults").innerHTML = `
    <h2>Uniquement dans A (mirroir vers B en cas d'update)</h2>
    ${renderComparisonTable(a, "a", bFolder)}
    <h2 style="margin-top:20px;">Uniquement dans B (mirroir vers A en cas d'update)</h2>
    ${renderComparisonTable(b, "b", aFolder)}`;
  document.getElementById("cmpApplySelected").disabled = a.length + b.length === 0;
}

document.getElementById("cmpRun").addEventListener("click", async () => {
  const folder_a = document.getElementById("cmpA").value.trim();
  const folder_b = document.getElementById("cmpB").value.trim();
  if (!folder_a || !folder_b) return alert("Renseigne les deux dossiers.");
  lastComparison = await runTask("Comparaison", () => invoke("compare_folders_cmd", { folderA: folder_a, folderB: folder_b }));
  renderComparisonResults();
});

async function applyAutoUpdate(entries, destRoot) {
  const config = readAutoUpdateConfig();
  await runTask("Auto update", () => invoke("auto_update_cmd", { entries, destRoot, config }));
  renderComparisonResults();
}

document.getElementById("cmpResults").addEventListener("click", async (e) => {
  const row = e.target.closest("tr[data-idx]");
  if (!row) return;
  const side = row.dataset.side;
  const idx = Number(row.dataset.idx);
  const entry = lastComparison[side === "a" ? "only_in_a" : "only_in_b"][idx];
  const destRoot = row.closest("table").dataset.destRoot;

  if (e.target.closest(".cmp-reveal")) {
    await invoke("reveal_in_file_manager_cmd", { path: entry.absolute_path });
  } else if (e.target.closest(".cmp-remove")) {
    lastComparison[side === "a" ? "only_in_a" : "only_in_b"].splice(idx, 1);
    renderComparisonResults();
  } else if (e.target.closest(".cmp-update")) {
    await applyAutoUpdate([entry], destRoot);
  }
});

document.getElementById("cmpApplySelected").addEventListener("click", async () => {
  const checked = [...document.querySelectorAll("#cmpResults .cmp-select:checked")];
  const bySide = { a: [], b: [] };
  for (const cb of checked) {
    const row = cb.closest("tr");
    bySide[row.dataset.side].push(Number(row.dataset.idx));
  }
  if (bySide.a.length) {
    const entries = bySide.a.map((i) => lastComparison.only_in_a[i]);
    await applyAutoUpdate(entries, document.getElementById("cmpB").value);
  }
  if (bySide.b.length) {
    const entries = bySide.b.map((i) => lastComparison.only_in_b[i]);
    await applyAutoUpdate(entries, document.getElementById("cmpA").value);
  }
});

// =======================================================================
// FIND DUPLICATE
// =======================================================================
let lastDuplicates = [];

function renderDuplicates() {
  document.getElementById("dupGroupCount").textContent = lastDuplicates.length;
  const wasted = lastDuplicates.reduce((sum, g) => sum + g.size * (g.paths.length - 1), 0);
  document.getElementById("dupWasted").textContent = humanSize(wasted);

  if (!lastDuplicates.length) {
    document.getElementById("dupResults").innerHTML = `<p class="empty-state">Aucun doublon trouvé.</p>`;
    return;
  }
  document.getElementById("dupResults").innerHTML = lastDuplicates.map((g, gi) => `
    <div class="card" style="margin-bottom:12px;" data-group="${gi}">
      <div class="row" style="justify-content:space-between;">
        <b>${humanSize(g.size)} × ${g.paths.length} copies</b>
        <button class="btn btn-ghost btn-sm dup-hide">Masquer la ligne</button>
      </div>
      <table class="results">
        <thead><tr><th>Garder</th><th>Chemin</th><th>Actions</th></tr></thead>
        <tbody>
          ${g.paths.map((p, pi) => `
            <tr>
              <td><input type="radio" name="keep-${gi}" value="${pi}" ${pi === 0 ? "checked" : ""} /></td>
              <td class="path-cell">${escapeHtml(p)}</td>
              <td class="actions-cell"><button class="btn btn-secondary btn-sm dup-reveal" data-path="${escapeHtml(p)}">Ouvrir l'emplacement</button></td>
            </tr>`).join("")}
        </tbody>
      </table>
      <div class="row" style="margin-top:10px;">
        <button class="btn btn-danger btn-sm dup-delete-others">Supprimer les autres (corbeille)</button>
      </div>
    </div>`).join("");
}

document.getElementById("dupRun").addEventListener("click", async () => {
  const roots = splitList(document.getElementById("dupRoots").value);
  if (!roots.length) return alert("Renseigne au moins un dossier.");
  const options = {
    min_size: Number(document.getElementById("dupMinSize").value) || 0,
    ignore_empty: document.getElementById("dupIgnoreEmpty").checked,
    extensions: splitList(document.getElementById("dupExts").value.replace(/;/g, ",")).flatMap((s) => s.split(",")).map((s) => s.trim()).filter(Boolean),
    modified_within_days: document.getElementById("dupDays").value ? Number(document.getElementById("dupDays").value) : null,
  };
  lastDuplicates = await runTask("Scan des doublons", () => invoke("find_duplicates_cmd", { roots, options }));
  renderDuplicates();
});

document.getElementById("dupResults").addEventListener("click", async (e) => {
  const card = e.target.closest("[data-group]");
  if (!card) return;
  const gi = Number(card.dataset.group);
  const group = lastDuplicates[gi];

  if (e.target.closest(".dup-hide")) {
    lastDuplicates.splice(gi, 1);
    renderDuplicates();
  } else if (e.target.closest(".dup-reveal")) {
    await invoke("reveal_in_file_manager_cmd", { path: e.target.closest(".dup-reveal").dataset.path });
  } else if (e.target.closest(".dup-delete-others")) {
    const keepIdx = Number(card.querySelector(`input[name="keep-${gi}"]:checked`).value);
    const toDelete = group.paths.filter((_, i) => i !== keepIdx);
    if (!confirm(`Supprimer définitivement (corbeille) ${toDelete.length} fichier(s) ?`)) return;
    await runTask("Suppression", () => invoke("delete_duplicates_cmd", { paths: toDelete }));
    lastDuplicates.splice(gi, 1);
    renderDuplicates();
  }
});

// =======================================================================
// OPTIMIZER
// =======================================================================
document.getElementById("optInPlace").addEventListener("change", (e) => {
  document.getElementById("optDestField").style.display = e.target.checked ? "none" : "flex";
});

document.getElementById("optRun").addEventListener("click", async () => {
  const source = document.getElementById("optSource").value.trim();
  if (!source) return alert("Renseigne une source.");
  const inPlace = document.getElementById("optInPlace").checked;
  const lossy = document.getElementById("optLossy").checked;
  const options = {
    source,
    destination: inPlace ? null : document.getElementById("optDest").value.trim(),
    kind: document.getElementById("optKind").value,
    quality: lossy ? { Lossy: Number(document.getElementById("optQuality").value) } : "Lossless",
    keep_backup: document.getElementById("optKeepBackup").checked,
  };
  const report = await runTask("Optimisation", () => invoke("run_optimizer_cmd", { options }));
  const savedTotal = report.processed.reduce((s, f) => s + (f.original_size - f.new_size), 0);
  document.getElementById("optResults").innerHTML = `
    <p><b>${report.processed.length}</b> fichier(s) réellement optimisé(s), espace gagné : <b>${humanSize(savedTotal)}</b>.</p>
    ${report.unchanged.length ? `<p class="subtitle">${report.unchanged.length} fichier(s) déjà optimaux, laissés inchangés (le réencodage n'aurait fait que les agrandir).</p>` : ""}
    ${report.skipped.length ? `<p class="subtitle">${report.skipped.length} fichier(s) ignoré(s) (type non concerné).</p>` : ""}
    ${report.errors.length ? `<p style="color:var(--danger)">${report.errors.length} erreur(s) : ${report.errors.map(escapeHtml).join(" · ")}</p>` : ""}
    ${report.backups_created.length ? `
      <div class="row" style="margin-top:10px;">
        <button class="btn btn-danger btn-sm" id="optCleanBackups">🗑️ Supprimer les ${report.backups_created.length} .bak créés (corbeille)</button>
      </div>` : ""}`;

  const cleanBtn = document.getElementById("optCleanBackups");
  if (cleanBtn) {
    cleanBtn.addEventListener("click", async () => {
      if (!confirm(`Envoyer ${report.backups_created.length} fichier(s) .bak à la corbeille ?\n\nFais ça seulement une fois que tu as vérifié que les fichiers optimisés sont corrects.`)) return;
      await runTask("Nettoyage des .bak", () => invoke("delete_duplicates_cmd", { paths: report.backups_created }));
      cleanBtn.remove();
    });
  }
});

// =======================================================================
// GETALL
// =======================================================================
document.getElementById("gaRun").addEventListener("click", async () => {
  const sources = splitList(document.getElementById("gaSources").value);
  const destination = document.getElementById("gaDest").value.trim();
  if (!sources.length || !destination) return alert("Renseigne au moins une source et une destination.");
  const categories = [...document.querySelectorAll(".ga-cat:checked")].map((c) => c.value);
  const options = {
    sources, destination, categories,
    extra_extensions: splitList(document.getElementById("gaExtra").value.replace(/;/g, ",")).flatMap((s) => s.split(",")).map((s) => s.trim()).filter(Boolean),
    mode: document.getElementById("gaMode").value,
    export_json_index: document.getElementById("gaJson").checked,
  };
  const report = await runTask("GetALL", () => invoke("run_getall_cmd", { options }));
  document.getElementById("gaResults").innerHTML = `
    <p><b>${report.gathered.length}</b> fichier(s) rassemblé(s) dans <span class="path-cell">${escapeHtml(destination)}</span>.</p>
    ${report.errors.length ? `<p style="color:var(--danger)">${report.errors.length} erreur(s).</p>` : ""}`;
});

// =======================================================================
// TRIPHOTOSVIDEO
// =======================================================================
document.getElementById("smRun").addEventListener("click", async () => {
  const photos_source = document.getElementById("smPhotos").value.trim();
  const destination = document.getElementById("smDest").value.trim();
  if (!photos_source || !destination) return alert("Renseigne le dossier photos et la destination.");
  const options = {
    photos_source,
    videos_source: document.getElementById("smVideos").value.trim() || null,
    destination,
    photos_destination: document.getElementById("smPhotosDest").value.trim() || null,
    videos_destination: document.getElementById("smVideosDest").value.trim() || null,
    mode: document.getElementById("smMode").value,
    dedupe_by_hash: document.getElementById("smDedupe").checked,
    separate_photos_videos: document.getElementById("smSeparate").checked,
    folder_prefix: document.getElementById("smPrefix").value || "",
  };
  const report = await runTask("Tri", () => invoke("run_sort_media_cmd", { options }));
  document.getElementById("smResults").innerHTML = `
    <p><b>${report.moved.length}</b> fichier(s) triés, <b>${report.duplicates_skipped}</b> doublon(s) ignoré(s), <b>${report.unclassified}</b> en NSP.</p>
    ${report.errors.length ? `<p style="color:var(--danger)">${report.errors.length} erreur(s).</p>` : ""}`;
});

// =======================================================================
// TAKEPHOTOONPHONE
// =======================================================================
function parseFoldersConfig(text) {
  return text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith("#"));
}

document.getElementById("adbFoldersLoad").addEventListener("click", async () => {
  const path = await invoke("pick_text_file_cmd");
  if (!path) return;
  try {
    const content = await invoke("read_text_file_cmd", { path });
    const folders = parseFoldersConfig(content);
    if (!folders.length) return alert("Le fichier ne contient aucun dossier valide (une ligne = un dossier, # pour commenter).");
    document.getElementById("adbFolders").value = folders.join(";");
  } catch (err) {
    alert(`Impossible de lire le fichier :\n${err}`);
  }
});

document.getElementById("adbFoldersSave").addEventListener("click", async () => {
  const folders = splitList(document.getElementById("adbFolders").value);
  if (!folders.length) return alert("Aucun dossier à enregistrer.");
  const path = await invoke("pick_save_text_file_cmd", { defaultName: "android_folders.txt" });
  if (!path) return;
  const content = `# Dossiers Android à exporter (un par ligne)\n${folders.join("\n")}\n`;
  try {
    await invoke("write_text_file_cmd", { path, content });
  } catch (err) {
    alert(`Impossible d'enregistrer le fichier :\n${err}`);
  }
});

document.getElementById("adbCutKind").addEventListener("change", (e) => {
  const kind = e.target.value;
  document.getElementById("adbCutDateField").style.display = kind.includes("date") ? "flex" : "none";
  document.getElementById("adbCutDaysField").style.display = kind.includes("days") ? "flex" : "none";
});

function readCutRule() {
  const kind = document.getElementById("adbCutKind").value;
  if (kind === "all") return { kind: "all" };
  if (kind === "on_or_after_date" || kind === "before_date") {
    return { kind, value: document.getElementById("adbCutDate").value };
  }
  return { kind, value: Number(document.getElementById("adbCutDays").value) };
}

document.getElementById("adbListBtn").addEventListener("click", async () => {
  const path = document.getElementById("adbPath").value.trim();
  if (path) await invoke("set_adb_path_cmd", { path });
  const select = document.getElementById("adbDevice");
  try {
    const devices = await runTask("Recherche des appareils", () => invoke("list_android_devices_cmd"));
    if (!devices.length) {
      select.innerHTML = `<option value="">Aucun appareil détecté</option>`;
    } else {
      select.innerHTML = devices.map((d) => {
        const label = d.model || d.serial;
        if (d.state === "device") {
          return `<option value="${escapeHtml(d.serial)}">${escapeHtml(label)}</option>`;
        }
        // Not usable yet — show why, don't just hide it. "unauthorized" is
        // by far the most common case: USB debugging is on, but the phone
        // is showing a popup asking to trust this computer that hasn't
        // been tapped yet.
        const hint = {
          unauthorized: "non autorisé — regarde l'écran du téléphone et accepte la demande",
          offline: "hors ligne — débranche/rebranche le câble",
        }[d.state] || `état : ${d.state}`;
        return `<option value="" disabled>${escapeHtml(label)} — ${hint}</option>`;
      }).join("");
    }
  } catch {
    select.innerHTML = `<option value="">Erreur ADB — vérifie le chemin / le câble / le débogage USB</option>`;
  }
});

document.getElementById("adbRun").addEventListener("click", async () => {
  const device_serial = document.getElementById("adbDevice").value;
  const destination = document.getElementById("adbDest").value.trim();
  if (!device_serial || !destination) return alert("Sélectionne un appareil et une destination.");
  const options = {
    device_serial,
    android_paths: splitList(document.getElementById("adbFolders").value),
    destination,
    mode: document.getElementById("adbMode").value,
    cut_rule: readCutRule(),
    pull_structure: document.getElementById("adbStructure").value,
    dedupe_by_hash: document.getElementById("adbDedupe").checked,
  };
  const report = await runTask("Récupération", () => invoke("run_photo_pull_cmd", { options }));
  document.getElementById("adbResults").innerHTML = `
    <p><b>${report.folders_pulled.length}</b> dossier(s) récupéré(s), <b>${report.files_cut_on_device}</b> fichier(s) coupé(s) sur le téléphone,
       <b>${report.duplicates_removed_locally}</b> doublon(s) supprimé(s) localement.</p>
    ${report.errors.length ? `<p style="color:var(--danger)">${report.errors.map(escapeHtml).join(" · ")}</p>` : ""}`;
});

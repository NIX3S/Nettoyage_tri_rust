Ce dossier est vide par défaut.

Lance scripts/vendor-tools.ps1 (Windows) pour le remplir avec des versions
portables de qpdf et ffmpeg — l'app les utilisera automatiquement à la place
d'un qpdf/ffmpeg installé sur le PATH (voir core/src/fs_utils.rs::resolve_tool).

Sans ce dossier rempli, l'Optimizer PDF/Vidéo continue de fonctionner s'il
trouve qpdf/ffmpeg sur le PATH système ; sinon il renvoie une erreur claire
au lieu d'échouer silencieusement.

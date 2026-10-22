# JustBetter Launcher

Launcher du serveur Minecraft **JustBetter** (Minecraft 1.21.1, NeoForge), écrit en Rust pour Windows et Linux.

- Connexion Microsoft par code (microsoft.com/link) : aucun mot de passe ne passe par le launcher.
- Java 21 téléchargé automatiquement (Eclipse Temurin).
- NeoForge installé avec l'installateur officiel.
- Mods, configs et ressources synchronisés depuis un `.mrpack` (format Modrinth), vérifiés par SHA-1.
- Connexion directe au serveur au lancement.

## Pour les joueurs

Télécharger le launcher dans les [Releases](../../releases) :

- Windows : `JustBetter-Launcher-windows.exe`
- Linux : `JustBetter-Launcher-linux` (`chmod +x` puis lancer)

Les fichiers du jeu sont dans `%APPDATA%\.justbetter` (Windows) ou `~/.local/share/justbetter` (Linux).

## Publier une mise à jour du pack

Le launcher lit [`pack.json`](pack.json) à chaque démarrage : pas besoin de recompiler pour changer de pack.

1. Exporter le pack en `.mrpack`.
2. Créer une release `pack-<version>` et y joindre le `.mrpack`.
3. Dans `pack.json`, mettre à jour `version` et `mrpack` (lien de la release), puis pousser sur `main`.

Au lancement suivant, les joueurs reçoivent les nouveaux mods. Les mods retirés du pack sont supprimés. Les configs du pack sont réécrites (sauf `options.txt`).

## Compiler

```sh
cargo build --release
```

Sous Linux : `pkg-config` et `libssl-dev`. Sous Windows avec la chaîne GNU, `dlltool` doit être dans le PATH.

Un tag `v*` lance la compilation Windows + Linux sur GitHub Actions et publie les exécutables dans la release.

Test sans compte (développement) : `justbetter-launcher --test-offline <pseudo>` avec la variable `JUSTBETTER_MANIFEST` pointant vers un `pack.json`.

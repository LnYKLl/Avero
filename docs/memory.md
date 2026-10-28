# Mesure de la mémoire

Cible : moins de 50 Mo au repos avec 4 comptes (objectif mesuré, pas une barrière).

## Rendu

Depuis le 2026-10-28, Avero utilise par défaut le **rendu logiciel tiny-skia** (dessin par le
processeur) : dans `crates/avero-app/Cargo.toml`, iced est déclaré sans ses fonctionnalités par
défaut, avec toutes sauf `wgpu`.

Le rendu GPU (wgpu) reste disponible avec la fonctionnalité `gpu` :

```
cargo build --release -p avero-app --features gpu
```

Avec `--features gpu`, iced essaie d'abord wgpu et se replie sur tiny-skia si aucun GPU n'est
utilisable.

## Mesure du 2026-10-28 (0 compte, tiny-skia par défaut)

- OS : Windows 11 Pro, build `cargo build --release -p avero-app` (sans `--features gpu`).
- Comptes : 0, base vide vérifiée (0 compte, 0 dossier, 0 message). Aucun compte réel n'est
  disponible sur cette machine.
- Méthode : lancement de `avero-app.exe`, 60 s d'inactivité, puis lecture de
  `Process.PrivateMemorySize64` (mémoire privée engagée) et `Process.WorkingSet64`.
  Deux exécutions.

| Exécution | Mémoire privée | Working set |
|---|---|---|
| 1 | 9,4 Mo | 24,5 Mo |
| 2 | 9,4 Mo | 25,3 Mo |

## Mesure du 2026-10-28 (interface V1, mode démo, ~40 messages)

- OS : Windows 11 Pro, build `cargo build --release -p avero-app` (tiny-skia).
- Lancement avec `AVERO_DEMO=1` : base en mémoire, 3 comptes fictifs, 40 messages dont
  les corps sont en cache, polices Inter et icônes SVG chargées, aucun réseau.
- Méthode : 60 s d'inactivité, puis `Process.PrivateMemorySize64` et `Process.WorkingSet64`.
  Deux exécutions.

| Exécution | Mémoire privée | Working set |
|---|---|---|
| 1 | 24,3 Mo | 42,7 Mo |
| 2 | 23,9 Mo | 42,5 Mo |

La hausse par rapport à la fenêtre vide vient surtout des quatre fichiers Inter, des icônes
rasterisées et du runtime tokio d'iced. Pas encore mesuré avec de vrais comptes synchronisés
(chaque compte ajoute un fil avec son propre runtime et sa connexion SQLite).

## Mesure précédente (même jour, avant le changement)

Avant ce changement, iced était compilé avec wgpu et l'utilisait par défaut :

| Rendu | Mémoire privée | Working set |
|---|---|---|
| wgpu (GPU), ancien défaut | 198 Mo | 160 Mo |
| tiny-skia (logiciel) | 10 Mo | 30 Mo |

Constat : le rendu GPU (wgpu) coûte à lui seul environ 130 à 190 Mo ; c'est lui qui faisait
dépasser la cible, d'où le passage à tiny-skia par défaut.

## À faire

4 comptes à mesurer : mesure à faire avec 4 comptes réels synchronisés.

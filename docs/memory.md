# Mesure de la mémoire

Cible : moins de 50 Mo au repos avec 4 comptes (objectif mesuré, pas une barrière).

## Mesure du 2026-10-28 (0 compte)

- OS : Windows 11 Pro, build `cargo build --release -p avero-app`.
- Comptes : 0, base présumée vide (~4 Ko). Aucun compte réel n'est disponible sur cette machine.
- Méthode : lancement de `avero-app.exe`, 60 s d'inactivité, puis lecture de
  `Process.PrivateMemorySize64` (mémoire privée engagée) et `Process.WorkingSet64`.
  Deux exécutions ont donné les mêmes valeurs à 3 % près.

| Rendu | Mémoire privée | Working set |
|---|---|---|
| Par défaut (wgpu, GPU) | 198 Mo | 160 Mo |
| Logiciel (`ICED_BACKEND=tiny-skia`) | 10 Mo | 30 Mo |

Le rendu logiciel est sélectionné par la variable d'environnement `ICED_BACKEND=tiny-skia`
(la fonctionnalité `tiny-skia` d'iced 0.14 est déjà activée par défaut, rien à changer dans `Cargo.toml`).
Le rendu par défaut n'a pas été modifié dans le code.

Constat : le rendu GPU (wgpu) coûte à lui seul environ 130 à 190 Mo ; c'est lui qui fait dépasser la cible.

## À faire

4 comptes à mesurer : mesure à faire avec 4 comptes réels synchronisés.

# Apprendre Rust avec Avero — Plan 1

Un concept Rust par étape du plan 1, avec l'endroit du code où le voir.

## Étape 1 — Structures, enums, modules (crates/avero-core/src/model.rs)

Une `struct` regroupe des champs nommés et une `enum` liste des variantes possibles (comme `FolderRole`). `#[derive(...)]` fait générer automatiquement des capacités comme `Debug`, `Clone` ou `PartialEq` par le compilateur. `pub` rend un élément visible hors de son module ; `pub use model::*` le réexporte depuis la racine de la crate. Les tests vivent dans `#[cfg(test)] mod tests`, compilé seulement pour `cargo test`. Les méthodes se définissent dans un bloc `impl`.

## Étape 2 — Result, ? et erreurs (crates/avero-core/src/store.rs)

`Result` et `?` : une fonction qui peut échouer renvoie `Result<T, E>` ; `?` renvoie l erreur tout de suite à l appelant, sinon donne la valeur. `&self` emprunte le Store sans le consommer. `thiserror` fabrique l enum d erreur, et `#[from]` convertit automatiquement rusqlite::Error en StoreError. Un `Vec` est un tableau dynamique qu on remplit avec push.

## Étape 3 — Option et slices (crates/avero-sync/src/parse.rs)

Option représente une valeur qui peut manquer (Some/None). as_deref transforme Option<String> en Option<&str> pour comparer facilement. let Some(x) = ... else { return } sort tôt si la valeur manque. Les slices &[u8] sont des vues sur des octets sans copie, et match sur &str permet de comparer un texte à plusieurs motifs avec |.

## Étape 4 — Traits, Send/Sync, Mutex (crates/avero-core/src/secrets.rs)

un trait (`SecretStore`) décrit un comportement que plusieurs types implémentent (coffre réel, version mémoire pour tests). `Send + Sync` garantit qu'on peut partager l'objet entre threads; `Mutex` permet de modifier le HashMap de façon sûre depuis plusieurs threads; `Box<dyn SecretStore>` permet de choisir l'implémentation à l'exécution.

## Étape 5 — async/await et généricité (crates/avero-sync/src/sync.rs)

`async fn` renvoie un "futur" : une valeur qui ne s'exécute que lorsqu'on l'`await`. `sync_account<S: MailSource>` est générique : elle accepte n'importe quel type qui implémente le trait, donc la vraie source IMAP ou la fausse source des tests. `&mut S` est un emprunt exclusif : la fonction peut modifier la source (curseur de sélection) sans en prendre possession, et l'appelant la retrouve ensuite.

## Étape 6 — Implémenter un trait, map_err (crates/avero-sync/src/imap.rs)

Ici on implémente un trait (`impl MailSource for ImapSource`) : le type promet de fournir les trois méthodes du trait, ce qui permet à `sync_account` de fonctionner aussi bien avec un faux serveur de test qu'avec le vrai. On utilise aussi `map_err` pour convertir les erreurs des bibliothèques externes en notre propre `SyncError`, et `?` pour les propager. Les fonctions `async` ne s'exécutent que lorsqu'on les `.await`, ce qui permet d'ajouter un délai maximum (`timeout`) autour de toute la connexion.

## Étape 7 — Threads et canaux (crates/avero-app/src/worker.rs)

Un `std::thread` indépendant reçoit ses données par `move` : elles lui appartiennent, d'où `'static`. Le futur retourné doit être `Send` pour passer d'un thread à l'autre ; le canal `oneshot` envoie le résultat du thread de synchro vers le futur. Chaque thread ouvre sa propre connexion SQLite, car une `Connection` ne se partage pas entre threads. Si le thread panique, l'émetteur est détruit et `receiver.await` renvoie `Err`, converti en message au lieu de propager la panique.

## Étape 8 — L'architecture d'iced (crates/avero-app/src/app.rs)

iced suit l'architecture « état → vue → message ». L'état (`App`) contient toutes les données. `view(&self)` les lit pour dessiner la fenêtre. Chaque clic produit un `Message`, et `update(&mut self, message)` est le seul endroit qui modifie l'état. On peut donc tester `update` sans ouvrir de fenêtre. Le type `Element<'a, Message>` porte une durée de vie `'a` : il signifie que la vue *emprunte* les données de l'état (le texte de l'objet, le nom de l'expéditeur) au lieu de les copier, et le compilateur vérifie que l'état vit plus longtemps que la vue. Les closures `move |theme, status| …` utilisées pour les styles capturent une copie des petites valeurs dont elles ont besoin (par exemple `active` ou la couleur du compte), ce qui leur permet de survivre après la fin de la fonction qui les a créées. Enfin, `Task::perform(future, |(id, r)| Message::SyncFinished(id, r))` confie un travail asynchrone (la synchronisation) à iced et transforme son résultat en message. L'interface ne se bloque donc jamais.

## Étape 9 — Validation et sécurité du mot de passe (crates/avero-app/src/view/add_account.rs)

La validation renvoie un `Option` : `validate` retourne `Some((compte, mot de passe))` si tout est bon, sinon `None` en remplissant `errors`. `parse::<u16>()` renvoie un `Result` : il refuse tout ce qui n'est pas un entier de 0 à 65535, donc "99999" échoue sans code en plus. Le mot de passe ne touche jamais la base : `NewAccount` n'a pas de champ pour lui, il part seul vers `SecretStore::set` (coffre-fort du système), et le test relit chaque colonne de la base pour le prouver.


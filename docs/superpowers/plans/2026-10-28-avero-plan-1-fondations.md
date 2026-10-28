# Avero — Plan 1 : Fondations — Plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Une première version qui tourne : ajouter un ou plusieurs comptes IMAP, synchroniser les en-têtes de leurs dossiers dans SQLite et les afficher dans une boîte unifiée (disposition B), filtrable par compte, avec la fusion des doublons.

**Architecture:** Un workspace Cargo de trois crates (`avero-core` : modèles, base SQLite, secrets ; `avero-sync` : IMAP et synchronisation ; `avero-app` : interface iced). La synchronisation tourne sur un thread dédié avec son propre runtime tokio et sa propre connexion SQLite. L'interface lit la base sur le thread principal.

**Tech Stack:** Rust 1.96 (édition 2024), iced 0.14, tokio 1.54, async-imap 0.12 (feature `runtime-tokio`), tokio-rustls 0.26 + rustls-native-certs 0.8, mail-parser 1.0, rusqlite 0.41 (feature `bundled`), keyring 4.2, directories, thiserror 2, anyhow 1.

**Spec:** `docs/superpowers/specs/2026-10-27-avero-design.md`

## Feuille de route (plans suivants, hors de ce plan)

1. **Fondations** (ce plan)
2. Lecture : corps des mails (30 jours puis à la demande), HTML → texte enrichi, images bloquées, pièces jointes, recherche FTS5 + IMAP `SEARCH`
3. Écriture : SMTP (`lettre`), réponse depuis le bon compte, brouillons serveur, file d'actions hors ligne
4. Comptes : autoconfig ISPDB, OAuth2 Microsoft, identifiant client Google personnel, mots de passe d'application, couleurs, groupes
5. Boîte unifiée avancée : tous les dossiers unifiés, actions groupées, vue par expéditeur, rappel plus tard, désinscription en un clic
6. Filtres : tri intelligent, anti-spam bayésien (`avero-filters`), grand nettoyage, blocage
7. Distribution : installateurs, mode portable + coffre chiffré, publication des binaires

## Global Constraints

- Licence : **GPL-3.0** (`license = "GPL-3.0-or-later"` dans chaque `Cargo.toml`, fichier `LICENSE` = texte officiel GPL-3.0).
- Nom de l'application : **Avero**. Nom du service keyring : `"avero"`.
- Multiplateforme : Windows, macOS, Linux. La CI compile et teste sur les trois.
- Pas d'Electron, pas de webview.
- Aucun mot de passe ni jeton en clair sur le disque : les secrets passent par `SecretStore` (Task 4).
- Rien du contenu des mails n'est envoyé ailleurs qu'au serveur du compte.
- Connexion IMAP toujours en TLS (port 993 par défaut).
- Cible mémoire : < 50 Mo de RAM au repos avec 4 comptes. C'est un objectif mesuré, pas un critère bloquant.
- Le porteur du projet apprend Rust. Code simple, pas de macros maison. Chaque tâche se termine par une ligne « Concept Rust » à lui expliquer, en français.
- `cargo fmt --check` et `cargo clippy --all-targets -- -D warnings` doivent passer à chaque commit.

## Review Focus

1. **En-têtes abîmés ou incomplets** (pas de `Message-ID`, pas de `Date`, objet encodé `=?UTF-8?B?…?=`, octets non UTF-8) : le mail s'affiche quand même, avec des valeurs de secours. Test : Task 3.
2. **Changement d'UIDVALIDITY** : le cache du dossier est vidé puis rechargé, sans doublons ni mails fantômes. Test : Task 5.
3. **Même `Message-ID` dans deux dossiers de types différents** (Envoyés de A, Réception de B) : pas de fusion. **Même `Message-ID` dans deux Réceptions** : une seule ligne, deux comptes. Test : Task 2.
4. **Serveur injoignable ou mauvais mot de passe** : l'erreur s'affiche sur ce compte, les autres continuent, l'appli ne plante pas. Test : Task 7.
5. **Grosse boîte (10 000+ mails) à la première synchro** : récupération par lots de 500 UID, liste paginée (200 lignes), l'interface reste réactive. Tests : Task 5 (lots) et Task 2 (limite/offset).

---

### Task 1 : Dépôt, workspace et modèles de base

**Files:**
- Create: `Cargo.toml` (workspace, `resolver = "3"`, membres `crates/*`)
- Create: `.gitignore` (`target/`, `.superpowers/`)
- Create: `LICENSE`, `README.md` (anglais : but, état « pre-alpha », compilation `cargo run -p avero-app`)
- Create: `.github/workflows/ci.yml`
- Create: `crates/avero-core/Cargo.toml`, `crates/avero-core/src/lib.rs`, `crates/avero-core/src/model.rs`

**Interfaces:**
- Produces (`avero_core::model`, réexporté par `lib.rs`) :
  - `pub struct AccountId(pub i64)` (dérive `Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord`)
  - `pub struct FolderId(pub i64)` (mêmes dérives)
  - `pub struct AccountColor { pub r: u8, pub g: u8, pub b: u8 }` avec `AccountColor::palette(index: usize) -> AccountColor`, qui parcourt en boucle 8 couleurs fixes (la première est `#3B82F6`)
  - `pub struct ServerConfig { pub host: String, pub port: u16, pub username: String }`
  - `pub struct NewAccount { pub name: String, pub email: String, pub imap: ServerConfig }`
  - `pub struct Account { pub id: AccountId, pub name: String, pub email: String, pub color: AccountColor, pub imap: ServerConfig }`
  - `pub enum FolderRole { Inbox, Sent, Drafts, Archive, Trash, Junk, Other }` avec `as_str(&self) -> &'static str` et `from_str(&str) -> FolderRole` (texte inconnu → `Other`)
  - `pub struct Folder { pub id: FolderId, pub account_id: AccountId, pub path: String, pub role: FolderRole, pub uid_validity: Option<u32>, pub last_seen_uid: Option<u32> }`
  - `pub struct NewMessage { pub uid: u32, pub message_id: Option<String>, pub subject: String, pub from_name: Option<String>, pub from_addr: String, pub date: i64, pub seen: bool }` (`date` en secondes Unix)
  - `pub enum AccountFilter { All, Only(AccountId) }`
  - `pub struct UnifiedItem { pub row_ids: Vec<i64>, pub accounts: Vec<AccountId>, pub subject: String, pub from_display: String, pub date: i64, pub seen: bool }`

- [ ] **Step 1 :** `git init`, créer le workspace, `.gitignore`, `LICENSE`, `README.md` et la crate `avero-core` vide.
- [ ] **Step 2 : Écrire les tests qui échouent** dans `model.rs` (`#[cfg(test)] mod tests`)

```rust
#[test]
fn folder_role_roundtrip() {
    for r in [FolderRole::Inbox, FolderRole::Sent, FolderRole::Drafts, FolderRole::Archive,
              FolderRole::Trash, FolderRole::Junk, FolderRole::Other] {
        assert_eq!(FolderRole::from_str(r.as_str()), r);
    }
    assert_eq!(FolderRole::from_str("nimporte"), FolderRole::Other);
}

#[test]
fn palette_cycles() {
    assert_eq!(AccountColor::palette(0), AccountColor { r: 0x3B, g: 0x82, b: 0xF6 });
    assert_eq!(AccountColor::palette(8), AccountColor::palette(0));
    assert_ne!(AccountColor::palette(1), AccountColor::palette(0));
}
```

- [ ] **Step 3 :** `cargo test -p avero-core` → FAIL (les types n'existent pas encore).
- [ ] **Step 4 :** Implémenter les types listés dans Interfaces.
- [ ] **Step 5 :** `cargo test -p avero-core` → PASS. Puis `cargo fmt --check` et `cargo clippy --all-targets -- -D warnings` → aucun avertissement.
- [ ] **Step 6 :** Écrire `ci.yml` : déclenché sur push et pull_request, matrice `ubuntu-latest`, `windows-latest`, `macos-latest`, toolchain stable, étapes fmt, clippy et test. Sous Ubuntu, installer d'abord `libdbus-1-dev pkg-config` (pour keyring).
- [ ] **Step 7 : Commit** (inclure aussi `docs/`)

```bash
git add -A
git commit -m "chore: workspace Avero, licence GPL-3.0, CI et modèles de base"
```

Concept Rust : `struct`, `enum`, `derive`, modules et `pub`, tests `#[cfg(test)]`.

---

### Task 2 : Base SQLite (`Store`)

**Files:**
- Create: `crates/avero-core/src/store.rs`
- Create: `crates/avero-core/src/schema.sql`
- Modify: `crates/avero-core/Cargo.toml` (ajouter `rusqlite` avec la feature `bundled`, et `thiserror`)

**Interfaces:**
- Consumes : tous les types de la Task 1.
- Produces (`avero_core::store`) :
  - `pub enum StoreError` (thiserror, enveloppe `rusqlite::Error`)
  - `pub struct Store` (possède une `rusqlite::Connection`)
  - `Store::open(path: &Path) -> Result<Store, StoreError>` (active `journal_mode=WAL`)
  - `Store::open_in_memory() -> Result<Store, StoreError>`
  - `add_account(&self, new: NewAccount) -> Result<Account, StoreError>` (couleur = `AccountColor::palette(nombre de comptes existants)`)
  - `accounts(&self) -> Result<Vec<Account>, StoreError>` (triés par id)
  - `upsert_folder(&self, account: AccountId, path: &str, role: FolderRole) -> Result<Folder, StoreError>` (unique sur `(account, path)` ; met le rôle à jour)
  - `folders(&self, account: AccountId) -> Result<Vec<Folder>, StoreError>`
  - `reset_folder(&self, folder: FolderId, uid_validity: u32) -> Result<(), StoreError>` (supprime les messages du dossier, enregistre la nouvelle validité, `last_seen_uid = NULL`)
  - `insert_messages(&self, folder: FolderId, msgs: &[NewMessage]) -> Result<(), StoreError>` (dans une transaction ; `(folder, uid)` existant ignoré ; `last_seen_uid = max(ancien, max uid inséré)`)
  - `unified(&self, role: FolderRole, filter: AccountFilter, limit: usize, offset: usize) -> Result<Vec<UnifiedItem>, StoreError>`

Schéma : tables `accounts`, `folders`, `messages(id, folder_id, uid, message_id, subject, from_name, from_addr, date, seen)`, avec `UNIQUE(folder_id, uid)` et un index sur `(message_id)`. Migrations : `PRAGMA user_version`. Version 1 = `schema.sql`, appliqué avec `include_str!`.

Algorithme de `unified` : garder les messages des dossiers de rôle `role` (et du compte, si le filtre est `Only`), puis grouper par `COALESCE(message_id, 'row:' || messages.id)`. Pour chaque groupe :
- `row_ids` et `accounts` = toutes les lignes du groupe, comptes dédoublonnés et triés ;
- `subject`, `from_display` et `date` = ceux de la ligne la plus récente ;
- `from_display` = `from_name` s'il existe, sinon `from_addr` ;
- `seen` = vrai seulement si toutes les copies sont lues.

Tri par `date` décroissante, puis application de `limit`/`offset`. Le regroupement peut se faire en Rust après une requête SQL triée.

- [ ] **Step 1 : Écrire les tests qui échouent** (`store.rs`, avec des helpers `acct(store, "a@x")` et `msg(uid, Some("<id>"), date)`) :

```rust
#[test] fn add_account_assigns_palette_colors() { /* 2 comptes → palette(0) puis palette(1) */ }

#[test]
fn same_message_id_in_two_inboxes_is_merged() {
    // A/INBOX et B/INBOX contiennent tous deux "<m1>"
    let items = s.unified(FolderRole::Inbox, AccountFilter::All, 200, 0).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].accounts, vec![a, b]);
    assert_eq!(items[0].row_ids.len(), 2);
}

#[test]
fn same_message_id_in_sent_and_inbox_is_not_merged() {
    // "<m1>" dans A/Sent et dans B/INBOX
    assert_eq!(s.unified(FolderRole::Inbox, AccountFilter::All, 200, 0).unwrap().len(), 1);
    assert_eq!(s.unified(FolderRole::Sent,  AccountFilter::All, 200, 0).unwrap().len(), 1);
}

#[test] fn missing_message_id_is_never_merged() { /* 2 messages sans message_id → 2 lignes */ }
#[test] fn filter_only_returns_one_account() { /* Only(a) → seulement les lignes de a */ }
#[test] fn sorted_newest_first_and_paginated() { /* 5 messages, limit 2 offset 2 → 3e et 4e plus récents */ }
#[test] fn insert_is_idempotent_and_tracks_last_uid() { /* insérer 2 fois uid 1..3 → 3 lignes, last_seen_uid == Some(3) */ }
#[test] fn reset_folder_clears_messages() { /* après reset → 0 ligne, uid_validity == Some(nouvelle), last_seen_uid == None */ }
#[test] fn merged_seen_only_if_all_copies_seen() { /* une copie lue, l'autre non → seen == false */ }
```

- [ ] **Step 2 :** `cargo test -p avero-core store` → FAIL.
- [ ] **Step 3 :** Implémenter `schema.sql` et `Store`.
- [ ] **Step 4 :** `cargo test -p avero-core` → PASS, puis fmt et clippy sans avertissement.
- [ ] **Step 5 : Commit** `feat(core): base SQLite et boîte unifiée avec fusion des doublons`

Concept Rust : `Result` et l'opérateur `?`, emprunts `&self`, `Vec`, erreurs avec `thiserror`.

---

### Task 3 : Analyse des en-têtes et rôle des dossiers

**Files:**
- Create: `crates/avero-sync/Cargo.toml`, `crates/avero-sync/src/lib.rs`, `crates/avero-sync/src/parse.rs`
- Create: `crates/avero-sync/tests/fixtures/` (quelques fichiers `.eml` d'en-têtes)

**Interfaces:**
- Consumes : `NewMessage`, `FolderRole`.
- Produces :
  - `avero_sync::parse::parse_headers(uid: u32, seen: bool, raw: &[u8]) -> NewMessage` (ne panique jamais)
  - `avero_sync::parse::detect_role(path: &str, attributes: &[String]) -> FolderRole`

Valeurs de secours de `parse_headers` :
- objet absent → `"(sans objet)"` ;
- expéditeur absent → `from_addr = ""` ;
- date absente ou illisible → `0` ;
- `message_id` stocké **avec** les chevrons `<…>`, ou `None`.

Utiliser `mail_parser::MessageParser::new().parse_headers(raw)`.

`detect_role` : regarder d'abord les attributs SPECIAL-USE (`\Sent`, `\Drafts`, `\Archive`, `\All` → Archive, `\Trash`, `\Junk`). Puis, sans tenir compte de la casse, le dernier segment du chemin : `INBOX` → Inbox ; `Sent`, `Sent Items`, `Sent Messages`, `Envoyés`, `Éléments envoyés` → Sent ; `Drafts`, `Brouillons` → Drafts ; `Archive`, `Archives` → Archive ; `Trash`, `Deleted Items`, `Corbeille`, `Bin` → Trash ; `Junk`, `Spam`, `Courrier indésirable` → Junk. Sinon → Other.

- [ ] **Step 1 : Écrire les tests qui échouent** (`parse.rs`) :

```rust
#[test]
fn parses_normal_headers() {
    let m = parse_headers(7, true, include_bytes!("../tests/fixtures/normal.eml"));
    assert_eq!(m.uid, 7);
    assert_eq!(m.message_id.as_deref(), Some("<abc@ex.com>"));
    assert_eq!(m.subject, "Réunion jeudi");
    assert_eq!(m.from_name.as_deref(), Some("Marie D."));
    assert_eq!(m.from_addr, "marie@ex.com");
    assert_eq!(m.date, 1_793_000_000); // la Date: de la fixture
}
#[test] fn decodes_encoded_word_subject() { /* "=?UTF-8?B?w4l0w6k=?=" → "Été" */ }
#[test] fn missing_headers_use_fallbacks() { /* b"X-Foo: bar\r\n\r\n" → "(sans objet)", "", 0, None */ }
#[test] fn garbage_bytes_do_not_panic() { /* &[0xff, 0xfe, 0x00, 0x80] → ne panique pas */ }
#[test] fn role_from_special_use() { assert_eq!(detect_role("[Gmail]/Messages envoyés", &["\\Sent".into()]), FolderRole::Sent); }
#[test] fn role_from_name() { assert_eq!(detect_role("INBOX.Corbeille", &[]), FolderRole::Trash); assert_eq!(detect_role("inbox", &[]), FolderRole::Inbox); }
#[test] fn unknown_is_other() { assert_eq!(detect_role("Factures", &[]), FolderRole::Other); }
```

- [ ] **Step 2 :** `cargo test -p avero-sync` → FAIL.
- [ ] **Step 3 :** Implémenter les deux fonctions.
- [ ] **Step 4 :** `cargo test -p avero-sync` → PASS, puis fmt et clippy sans avertissement.
- [ ] **Step 5 : Commit** `feat(sync): analyse des en-têtes et détection du rôle des dossiers`

Concept Rust : `Option`, `as_deref`, slices `&[u8]`, `match` sur des chaînes.

---

### Task 4 : Secrets (`SecretStore`)

**Files:**
- Create: `crates/avero-core/src/secrets.rs`
- Modify: `crates/avero-core/Cargo.toml` (ajouter `keyring` 4.2, avec les features des coffres natifs Windows, macOS et Linux Secret Service : à vérifier dans la doc de la 4.2)

**Interfaces:**
- Produces :
  - `pub enum SecretError` (thiserror)
  - `pub trait SecretStore: Send + Sync { fn get(&self, account: AccountId) -> Result<Option<String>, SecretError>; fn set(&self, account: AccountId, secret: &str) -> Result<(), SecretError>; fn delete(&self, account: AccountId) -> Result<(), SecretError>; }`
  - `pub struct KeyringSecrets` (service `"avero"`, utilisateur `format!("account-{}", id.0)`)
  - `pub struct MemorySecrets` (un `Mutex<HashMap<AccountId, String>>`, pour les tests)

- [ ] **Step 1 : Écrire les tests qui échouent** : `memory_set_get_delete` (set → get `Some`, delete → get `None`, delete d'une entrée absente → `Ok`). Plus `keyring_roundtrip`, marqué `#[ignore]` parce qu'il touche le vrai coffre-fort de la machine. On le lance à la main avec `cargo test -- --ignored`.
- [ ] **Step 2 :** `cargo test -p avero-core secrets` → FAIL.
- [ ] **Step 3 :** Implémenter. Pour `KeyringSecrets::get`, une entrée absente renvoie `Ok(None)` et non une erreur.
- [ ] **Step 4 :** `cargo test -p avero-core` → PASS. Puis, une fois à la main sur la machine de dev : `cargo test -p avero-core -- --ignored keyring_roundtrip` → PASS.
- [ ] **Step 5 : Commit** `feat(core): stockage des mots de passe dans le coffre-fort du système`

Concept Rust : les traits, `Send + Sync`, `Mutex`, `Box<dyn Trait>`.

---

### Task 5 : Synchronisation (logique, testée avec une fausse source)

**Files:**
- Create: `crates/avero-sync/src/source.rs`, `crates/avero-sync/src/sync.rs`

**Interfaces:**
- Consumes : `Store` (Task 2), `parse_headers` et `detect_role` (Task 3).
- Produces :
  - `pub struct RemoteFolder { pub path: String, pub attributes: Vec<String> }`
  - `pub struct FolderStatus { pub uid_validity: u32, pub uid_next: Option<u32> }`
  - `pub struct FetchedHeader { pub uid: u32, pub seen: bool, pub raw: Vec<u8> }`
  - `pub enum SyncError` (thiserror : `Connect(String)`, `Auth(String)`, `Protocol(String)`, `Store(#[from] StoreError)`)
  - `pub trait MailSource { async fn list_folders(&mut self) -> Result<Vec<RemoteFolder>, SyncError>; async fn select(&mut self, path: &str) -> Result<FolderStatus, SyncError>; async fn fetch_headers(&mut self, from_uid: u32, to_uid: Option<u32>) -> Result<Vec<FetchedHeader>, SyncError>; }` (`async fn` natif dans le trait, sans `async_trait`. `to_uid = None` signifie `*`.)
  - `pub const BATCH: u32 = 500;`
  - `pub struct SyncReport { pub folders: usize, pub new_messages: usize }`
  - `pub async fn sync_account<S: MailSource>(store: &Store, source: &mut S, account: AccountId) -> Result<SyncReport, SyncError>`

Algorithme pour chaque dossier distant :
1. `upsert_folder` avec `detect_role`.
2. `select`. Si `uid_validity` diffère de celle stockée (et qu'il y en avait une), ou si aucune n'est stockée → `reset_folder`.
3. Récupérer par tranches de `BATCH` UID : `fetch_headers(start, Some(start + BATCH - 1))`, en partant de `last_seen_uid + 1` (ou de 1) et en avançant de `BATCH` tant que `start < uid_next`. Les tranches vides au milieu sont normales (UID clairsemés). Si le serveur ne donne pas `uid_next`, faire un seul `fetch_headers(start, None)`.
4. `insert_messages` après chaque tranche (la progression est sauvegardée si la synchro est interrompue).

Les dossiers `\Noselect` sont ignorés.

- [ ] **Step 1 : Écrire les tests qui échouent** (`sync.rs`) avec une `FakeSource` : dossiers en mémoire, compteur d'appels à `fetch_headers`, en-têtes générés avec `Message-ID: <n@fake>`.

```rust
#[tokio::test] async fn first_sync_imports_all_folders_and_roles() { /* INBOX(3 msgs) + Sent(1) → report.new_messages == 4, rôles Inbox/Sent */ }
#[tokio::test] async fn second_sync_only_fetches_new_uids() { /* ajouter uid 4 → new_messages == 1 */ }
#[tokio::test] async fn uidvalidity_change_resets_folder() { /* changer validité + renuméroter → total == nb de msgs distants, pas de doublon */ }
#[tokio::test] async fn large_folder_is_fetched_in_batches() { /* 1203 msgs → 1203 importés, chaque appel demande <= BATCH uids */ }
#[tokio::test] async fn noselect_folders_are_skipped() { /* attribut "\\Noselect" → aucun select sur ce chemin */ }
#[tokio::test] async fn sparse_uids_are_not_missed() { /* uids 1, 2, 900, 1700 → 4 importés */ }
```

- [ ] **Step 2 :** `cargo test -p avero-sync sync` → FAIL.
- [ ] **Step 3 :** Implémenter `source.rs` (types et trait) et `sync_account`.
- [ ] **Step 4 :** `cargo test -p avero-sync` → PASS, puis fmt et clippy sans avertissement.
- [ ] **Step 5 : Commit** `feat(sync): synchronisation incrémentale par lots avec gestion d'UIDVALIDITY`

Concept Rust : `async`/`await`, la généricité `<S: MailSource>`, `&mut`.

---

### Task 6 : Client IMAP réel (`ImapSource`)

**Files:**
- Create: `crates/avero-sync/src/imap.rs`
- Create: `crates/avero-sync/tests/live_imap.rs`

**Interfaces:**
- Consumes : `MailSource`, `ServerConfig`.
- Produces : `pub struct ImapSource` avec `pub async fn connect(cfg: &ServerConfig, password: &str) -> Result<ImapSource, SyncError>` et `impl MailSource for ImapSource`.

Choix techniques :
- TLS via `tokio-rustls`, avec les certificats racine de `rustls-native-certs`.
- `async-imap` avec la feature `runtime-tokio`.
- Erreur de connexion TCP/TLS → `SyncError::Connect` ; refus du `LOGIN` → `SyncError::Auth`.
- `fetch_headers` envoie `UID FETCH a:b (UID FLAGS BODY.PEEK[HEADER])`, avec `seen` = présence de `\Seen`.
- `list_folders` envoie `LIST "" "*"` et convertit les attributs en chaînes (`"\\Sent"`, `"\\Noselect"`…).

- [ ] **Step 1 : Écrire le test** `live_sync_against_real_server`, marqué `#[ignore]`. Il lit `AVERO_TEST_IMAP_HOST`, `AVERO_TEST_IMAP_USER` et `AVERO_TEST_IMAP_PASS`, synchronise dans un `Store::open_in_memory()`, puis vérifie `report.folders >= 1` et qu'il existe un dossier de rôle `Inbox`.
- [ ] **Step 2 : Écrire le test non ignoré** `bad_host_is_connect_error` : `connect` vers `127.0.0.1:1` → `Err(SyncError::Connect(_))` en moins de 10 s (timeout tokio).
- [ ] **Step 3 :** `cargo test -p avero-sync imap` → FAIL.
- [ ] **Step 4 :** Implémenter `ImapSource`.
- [ ] **Step 5 :** `cargo test -p avero-sync` → PASS. Puis, à la main, avec un vrai compte (mot de passe d'application Gmail, ou un compte Outlook/OVH) : `cargo test -p avero-sync -- --ignored live_sync` → PASS.
- [ ] **Step 6 : Commit** `feat(sync): client IMAP TLS`

Concept Rust : intégrer des bibliothèques externes, `impl Trait for Type`, la conversion des erreurs avec `map_err`.

---

### Task 7 : Lancer la synchro en arrière-plan (`worker`)

**Files:**
- Create: `crates/avero-app/Cargo.toml`, `crates/avero-app/src/main.rs` (pour l'instant, juste `fn main` qui ouvre la base), `crates/avero-app/src/worker.rs`, `crates/avero-app/src/paths.rs`

**Interfaces:**
- Consumes : `Store`, `SecretStore`, `ImapSource`, `sync_account`.
- Produces :
  - `paths::database_path() -> PathBuf` (`directories::ProjectDirs::from("org", "Avero", "Avero")` → `data_dir()/avero.db`, en créant le dossier)
  - `worker::spawn_sync(db_path: PathBuf, account: Account, password: String) -> impl Future<Output = (AccountId, Result<SyncReport, String>)> + Send + 'static`

`spawn_sync` démarre un `std::thread` qui crée un runtime tokio `current_thread`. Ce runtime ouvre **son propre** `Store::open(&db_path)`, puis appelle `ImapSource::connect` et `sync_account`. Le résultat revient par un `tokio::sync::oneshot`, que le futur retourné attend. Une erreur devient un message en français pour l'utilisateur, par exemple `"Connexion impossible à imap.ex.com : …"` ou `"Identifiants refusés pour a@x"`.

- [ ] **Step 1 : Écrire les tests qui échouent** (`worker.rs`) :
  - `unreachable_server_reports_error_without_panicking` : compte vers `127.0.0.1:1` → `Err(msg)` avec `msg` qui contient `"Connexion impossible"`.
  - `two_accounts_fail_independently` : deux `spawn_sync` vers des hôtes invalides, joints avec `futures::join!` → deux `Err`, aucun panic.
- [ ] **Step 2 :** `cargo test -p avero-app worker` → FAIL.
- [ ] **Step 3 :** Implémenter `paths` et `worker`.
- [ ] **Step 4 :** `cargo test -p avero-app` → PASS, puis fmt et clippy sans avertissement.
- [ ] **Step 5 : Commit** `feat(app): synchronisation en arrière-plan sur un thread dédié`

Concept Rust : les threads, les canaux, `Send` et `'static`, pourquoi chaque thread a sa propre connexion SQLite.

---

### Task 8 : Interface — disposition B et boîte unifiée

**Files:**
- Create: `crates/avero-app/src/app.rs`, `crates/avero-app/src/view/mod.rs`, `crates/avero-app/src/view/rail.rs`, `crates/avero-app/src/view/list.rs`, `crates/avero-app/src/view/reader.rs`
- Modify: `crates/avero-app/src/main.rs` (lance `iced::application` avec le titre `"Avero"`)

**Interfaces:**
- Consumes : `Store`, `SecretStore`, `UnifiedItem`, `AccountFilter`, `spawn_sync`.
- Produces :
  - `pub enum Message { FilterSelected(AccountFilter), MessageSelected(usize), SyncAll, SyncFinished(AccountId, Result<SyncReport, String>), LoadMore, /* + messages du formulaire en Task 9 */ }`
  - `pub struct App { store: Store, secrets: Box<dyn SecretStore>, db_path: PathBuf, accounts: Vec<Account>, filter: AccountFilter, items: Vec<UnifiedItem>, selected: Option<usize>, errors: HashMap<AccountId, String>, syncing: HashSet<AccountId> }`
  - `App::new(store: Store, secrets: Box<dyn SecretStore>, db_path: PathBuf) -> (App, Task<Message>)` (charge les comptes, puis la page 1 de la Réception unifiée, puis lance `SyncAll`)
  - `App::update(&mut self, msg: Message) -> Task<Message>` et `App::view(&self) -> Element<'_, Message>`
  - `pub const PAGE: usize = 200;`

Disposition B (spec §5) :
- **Rail** de 48 px à gauche : boutons Mails (actif), Calendrier (désactivé, info-bulle « Bientôt »), Grand nettoyage (désactivé), Réglages en bas.
- **Liste** de 360 px :
  - en haut, la rangée de filtres « Tous » + une pastille ronde par compte, à sa couleur ;
  - les onglets Important / Newsletters / Notifications sont affichés mais inactifs (Plan 6) ;
  - chaque ligne montre les pastilles de `accounts`, `from_display` en gras si non lu, l'objet et la date (« 14:32 » si c'est aujourd'hui, « 27 oct. » sinon) ;
  - « Charger plus » en bas.
- **Lecteur** : en-têtes du mail sélectionné, et le texte « Le contenu du mail arrive dans la prochaine version » (Plan 2).

Une erreur de compte s'affiche sous sa pastille (pastille barrée + info-bulle avec le message).

- [ ] **Step 1 : Écrire les tests qui échouent** (`app.rs`, avec `Store::open_in_memory()` + `MemorySecrets`, sans aucune fenêtre) :

```rust
#[test] fn starts_on_all_accounts_unified_inbox() { /* 2 comptes avec mails → filter == All, items de 2 comptes */ }
#[test] fn selecting_account_filters_list() { /* update(FilterSelected(Only(a))) → tous les items ont a dans accounts, selected == None */ }
#[test] fn sync_error_is_recorded_per_account() { /* update(SyncFinished(a, Err("x"))) → errors[a] == "x", b sans erreur, items toujours chargés */ }
#[test] fn sync_success_reloads_and_clears_error() { /* erreur puis Ok → errors ne contient plus a */ }
#[test] fn load_more_appends_next_page() { /* 450 msgs → 200, LoadMore → 400, LoadMore → 450 */ }
```

- [ ] **Step 2 :** `cargo test -p avero-app app` → FAIL.
- [ ] **Step 3 :** Implémenter `App`, `update`, puis les vues.
- [ ] **Step 4 :** `cargo test -p avero-app` → PASS, puis fmt et clippy sans avertissement.
- [ ] **Step 5 : Vérification visuelle** : `cargo run -p avero-app` avec la base de la Task 6 (ou une vide). La fenêtre s'ouvre en disposition B, sans plantage, et le filtre par compte fonctionne.
- [ ] **Step 6 : Commit** `feat(app): écran principal disposition B avec boîte unifiée`

Concept Rust : l'architecture d'iced (état → vue → message), les durées de vie dans `Element<'_, _>`, les closures.

---

### Task 9 : Ajouter un compte et mesurer la mémoire

**Files:**
- Create: `crates/avero-app/src/view/add_account.rs`
- Modify: `crates/avero-app/src/app.rs` (messages du formulaire)
- Create: `docs/memory.md`

**Interfaces:**
- Consumes : `Store::add_account`, `SecretStore::set`, `spawn_sync`.
- Produces : messages `AddAccountOpened`, `AddAccountField(Field, String)` avec `pub enum Field { Name, Email, Host, Port, Username, Password }`, `AddAccountSubmitted`, `AddAccountCancelled`.

Formulaire (manuel, l'autoconfig viendra au Plan 4) :
- champs Nom, Adresse e-mail, Serveur IMAP, Port (993 par défaut), Identifiant (prérempli avec l'adresse), Mot de passe (masqué) ;
- un bouton « + » sous les pastilles des comptes ouvre le formulaire, et sa présence est indiquée quand il n'y a aucun compte ;
- à la validation : `add_account`, puis `secrets.set`, puis la synchro de ce compte.

Validation : e-mail avec `@`, serveur non vide, port de 1 à 65535, mot de passe non vide. Sinon, un message d'erreur sous le champ concerné, et aucun enregistrement.

- [ ] **Step 1 : Écrire les tests qui échouent** :
  - `valid_form_creates_account_and_stores_secret` : après Submitted, `accounts.len() == 1` et `MemorySecrets.get(id) == Some(pass)`.
  - `invalid_port_is_rejected` : port `"99999"` → aucun compte créé, erreur sur `Field::Port`.
  - `password_never_written_to_database` : après création, aucune colonne de la base ne contient la chaîne du mot de passe. Requête sur chaque table texte.
- [ ] **Step 2 :** `cargo test -p avero-app` → FAIL.
- [ ] **Step 3 :** Implémenter le formulaire et la logique.
- [ ] **Step 4 :** `cargo test --workspace` → PASS, puis fmt et clippy sans avertissement.
- [ ] **Step 5 : Mesure mémoire** : `cargo run --release -p avero-app` avec 1 à 4 comptes réels synchronisés. Relever la mémoire au repos (Gestionnaire des tâches sur Windows, colonne « Mémoire » = working set privé) après 1 minute d'inactivité. Noter dans `docs/memory.md` la date, l'OS, le nombre de comptes et de mails, et la valeur. Si elle dépasse 50 Mo, refaire la mesure avec le rendu logiciel d'iced (`ICED_BACKEND=tiny-skia`) et noter les deux.
- [ ] **Step 6 : Commit** `feat(app): ajout de compte IMAP et première mesure mémoire`

Concept Rust : la validation avec `Result`, `parse::<u16>()`, et pourquoi le mot de passe ne touche jamais la base.

---

## Couverture de la spec par ce plan

Ce plan couvre :
- plusieurs comptes IMAP ;
- la boîte unifiée (Réception) avec un filtre par compte et une couleur par compte ;
- la fusion des doublons selon les règles de la spec ;
- le cache local et les secrets dans le coffre-fort du système ;
- la disposition B ;
- la licence GPL-3.0 et la CI sur les trois OS ;
- la mesure de la cible mémoire.

Tout le reste de la V1 est réparti dans les plans 2 à 7 de la feuille de route ci-dessus. Chacun sera écrit au moment de le commencer, en tenant compte de ce que ce plan aura appris.

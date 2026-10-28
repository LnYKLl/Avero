# Avero — Document de conception

Date : 2026-10-27
Statut : brouillon à valider

## 1. Intention

Avero est un client mail de bureau, écrit en Rust, open source (GPL-3.0) et publié sur GitHub. C'est une alternative à Thunderbird qui doit :

- **consommer beaucoup moins de mémoire** ;
- permettre de connecter **autant de comptes mail qu'on veut** ;
- afficher **tous les mails ensemble** (boîte unifiée) ou **compte par compte**.

Le porteur du projet ne connaît pas encore bien Rust et veut l'apprendre en construisant Avero. Le code doit donc être **lisible, expliqué et découpé en petites étapes compréhensibles**. Cela compte autant que la performance.

## 2. Objectifs, non-objectifs, anti-objectifs

### Objectifs
- Fonctionner sur Windows, macOS et Linux.
- Rester léger. Cible (choix de Claude) : **moins de 50 Mo de RAM** au repos avec 4 comptes synchronisés, et une fenêtre affichée en moins d'une seconde au démarrage. C'est un **objectif mesuré**, pas un critère bloquant : la mémoire est mesurée dès les premières étapes et suivie en CI. Si le rendu GPU d'iced (wgpu) coûte trop cher, on essaie son rendu logiciel (tiny-skia). On ne change pas de bibliothèque d'interface pour ça.
- Offrir une boîte unifiée qui va plus loin que Thunderbird (voir §4).
- Aider à se débarrasser des mails indésirables (anti-spam et grand nettoyage).
- Être un projet open source accueillant : facile à compiler, à comprendre et à faire évoluer.

### Non-objectifs (pas pour la V1)
- Calendrier (prévu en V3).
- Contacts, PGP, extensions (plus tard).
- POP3, Exchange EWS, version mobile, version web.

### Anti-objectifs (ce serait un échec même si ça marche)
- Une appli lourde : pas d'Electron, pas de webview embarquée pour l'interface.
- L'envoi du contenu des mails à un serveur tiers (tri, anti-spam et recherche se font **uniquement en local**).
- Des mots de passe ou jetons stockés en clair sur le disque.
- Un code « magique » incompréhensible pour un débutant en Rust.

## 3. Contraintes et choix techniques

### Décidé avec le porteur du projet
- Langage : **Rust**.
- Licence : **GPL-3.0**.
- Nom : **Avero**.
- Multiplateforme : Windows, macOS, Linux.
- Disposition de l'écran principal : **option B**, décrite au §5.
- Calendrier en V3, après la V1 de la messagerie.

### Choix de Claude (proposés et non contestés)
| Domaine | Choix |
|---|---|
| Interface | `iced` (100 % Rust, architecture état → vue → messages) |
| Asynchrone | `tokio` |
| Réception | `async-imap` (+ `rustls` pour TLS) |
| Envoi | `lettre` (SMTP) |
| Analyse des mails | `mail-parser` |
| OAuth2 | `oauth2` (flux navigateur + redirection localhost, PKCE) |
| Stockage local | SQLite (`rusqlite`), recherche via **FTS5** |
| Secrets | `keyring` (coffre-fort du système) |
| Affichage des mails HTML | rendu simplifié du HTML en texte enrichi dans iced. **Pas de webview.** Images distantes bloquées par défaut (vie privée et mémoire), avec un bouton « afficher les images ». |

### Organisation du code (choix de Claude)
Un workspace Cargo avec des crates séparées, pour que chaque morceau reste petit et testable :
- `avero-core` : modèles (compte, dossier, message), base SQLite, recherche.
- `avero-sync` : IMAP/SMTP, OAuth2, synchronisation en arrière-plan.
- `avero-filters` : tri intelligent, anti-spam, détection des doublons.
- `avero-app` : interface iced et point d'entrée.

## 4. Fonctionnalités par version

### V1 — La messagerie

**Comptes**
- Ajout illimité de comptes IMAP/SMTP, avec détection automatique des serveurs (base Mozilla ISPDB / autoconfig, choix de Claude), et saisie manuelle en secours.
- OAuth2 pour Gmail et Microsoft (Outlook/Hotmail/365). Mot de passe d'application en secours.
- Couleur choisie par compte.
- **Groupes de comptes** (par ex. « Perso », « Pro ») créés par l'utilisateur.

**Boîte unifiée**
- Vue « Tous les comptes », vue par groupe, vue par compte.
- **Dossiers unifiés** : Réception, Envoyés, Brouillons, Archives, Corbeille (repérés grâce aux attributs IMAP SPECIAL-USE, puis aux noms courants).
- Pastille de couleur du compte sur chaque mail.
- **Réponse depuis le bon compte** : l'adresse d'envoi est choisie automatiquement selon l'adresse qui a reçu le mail.
- **Doublons fusionnés** : un mail reçu sur plusieurs comptes (même `Message-ID`) n'apparaît qu'une fois, avec les pastilles de tous les comptes concernés. Règles (choix de Claude) :
  - la fusion n'est qu'un **affichage**. La base garde une ligne par copie serveur, rattachée à un message logique ;
  - on ne fusionne qu'à l'intérieur d'un **même type de dossier unifié**. Un mail envoyé de A vers B reste donc dans les Envoyés de A et dans la Réception de B ;
  - une action sur un mail fusionné (lu, archiver, supprimer) s'applique **à toutes les copies affichées**, puisque l'utilisateur voit les pastilles de chaque compte. Le volet de lecture permet aussi d'agir sur une seule copie.
- **Actions groupées** (archiver, supprimer, marquer lu) sur une sélection qui mélange plusieurs comptes.
- **Vue par expéditeur** : tous les mails d'une personne, tous comptes confondus.
- **Rappel plus tard (snooze)** : le mail est masqué jusqu'à l'heure choisie. État stocké localement (choix de Claude : on ne déplace rien sur le serveur).
- **Désinscription en un clic** via l'en-tête `List-Unsubscribe` (mailto ou HTTP, y compris le one-click RFC 8058).
- **Tri intelligent** en onglets Important / Newsletters / Notifications. En V1 : règles simples et locales fondées sur les en-têtes (`List-Unsubscribe`, `List-Id`, `Precedence: bulk`, expéditeurs `noreply`…). Pas d'IA.

**Mails indésirables**
- **Filtre anti-spam local** (bayésien), qui apprend des boutons « Spam » / « Pas spam ». Il respecte aussi le dossier spam du serveur.
- **Écran « Grand nettoyage »** : liste des expéditeurs triés par volume, avec le nombre de mails, le taux de mails jamais ouverts et la taille totale. Pour chacun : tout supprimer, se désinscrire, bloquer (les futurs mails vont directement à la corbeille).
- Effet sur le serveur (choix de Claude) : ces actions **modifient le serveur**, pour que le résultat se voie aussi sur le téléphone.
  - « Spam » déplace le mail dans le dossier Junk du compte, « Pas spam » le remet en Réception.
  - « Tout supprimer » déplace dans la Corbeille du serveur.
  - « Bloquer » est une règle locale, appliquée à chaque synchronisation : elle n'agit que quand Avero tourne. On le signale dans l'interface.
  - Les actions faites hors ligne sont mises en file d'attente et rejouées à la reconnexion.

**Lecture et écriture**
- Lire, écrire, répondre, répondre à tous, transférer.
- Pièces jointes (ouvrir, enregistrer, joindre).
- Brouillons enregistrés sur le serveur.

**Stockage et recherche**
- Cache local SQLite : démarrage instantané et lecture hors ligne.
- Recherche plein texte dans tous les comptes. Choix de Claude : la recherche locale FTS5 porte sur tout ce qui est en cache (tous les en-têtes, et les corps téléchargés). Pour les mails plus anciens, un bouton « chercher aussi sur le serveur » lance un IMAP `SEARCH` sur chaque compte. Ce n'est pas un tiers, donc l'anti-objectif est respecté.
- Synchronisation (choix de Claude) : en-têtes complets, corps téléchargés pour les 30 derniers jours puis à la demande, pièces jointes à la demande.

> Note de Claude : la V1 est copieuse. Si on veut sortir plus vite, le tri intelligent et l'anti-spam bayésien peuvent glisser en V1.5 sans casser le reste.

### V2
- Notifications de nouveaux mails en temps réel (IMAP IDLE).
- Conversations regroupées en fils.
- Thème clair/sombre, raccourcis clavier.
- Signatures par compte.
- Règles et filtres définis par l'utilisateur.

### V3 — Calendrier
- Tous les agendas des comptes réunis (Google Agenda, Microsoft 365, CalDAV).
- Une couleur par compte, un filtre par compte ou par groupe.
- Invitations `.ics` reçues par mail, acceptées en un clic.
- Question ouverte, à trancher avant la V3 : détecter aussi les événements cachés dans le contenu des mails (billets, réservations) ?

### Plus tard
- Chiffrement PGP, contacts (CardDAV), extensions communautaires.

## 5. Écran principal (disposition B)

- **À gauche, une fine barre d'icônes** : Mails, Calendrier (grisé avant la V3), Grand nettoyage, Réglages en bas.
- **Au milieu, la liste des mails** :
  - en haut, des **filtres rapides** : « Tous », une pastille par compte, puis les groupes (Perso, Pro…) ;
  - en dessous, les **onglets** Important / Newsletters / Notifications ;
  - les dossiers unifiés (Réception, Envoyés…) sont accessibles depuis un sélecteur en haut de la liste (choix de Claude, à ajuster en construisant) ;
  - chaque ligne affiche la pastille du compte, l'expéditeur, l'objet, un extrait et la date.
- **À droite, le mail ouvert**, sur la plus grande zone possible.

## 6. Open source

- Dépôt GitHub public, licence GPL-3.0 (fichier `LICENSE`).
- `README.md` en anglais (choix de Claude, pour toucher plus de contributeurs), avec une traduction française possible.
- `CONTRIBUTING.md`, modèles d'issues et de PR.
- CI GitHub Actions : `cargo fmt`, `cargo clippy` et `cargo test` sur Windows, macOS et Linux.
- Binaires publiés automatiquement à chaque tag de version.

### Distribution (demandé par le porteur du projet : installateur + version portable)
- **Installateurs** : Windows `.msi` (ou `.exe` Inno Setup), macOS `.dmg`, Linux `.deb`, `.rpm` et AppImage. Flathub plus tard.
- **Version portable** : `.zip` Windows avec `avero.exe`, et l'AppImage sous Linux.
  - Choix de Claude : le mode portable est activé par un fichier marqueur `portable` placé à côté de l'exécutable. Les données (base SQLite, réglages) sont alors stockées dans un dossier `data/` à côté de l'exécutable.
  - En mode portable, les secrets ne vont **pas** dans le coffre-fort du système. Ils vont dans un fichier chiffré (dérivation de clé Argon2 + chiffrement authentifié) protégé par un **mot de passe principal** demandé au démarrage.

### Point d'attention : OAuth2 Gmail
Pour accéder à Gmail en IMAP, Google exige une application OAuth enregistrée. Le scope `https://mail.google.com/` est « restreint » : une appli publique doit passer une vérification de Google, avec un audit de sécurité pour une diffusion large. **Décidé avec le porteur du projet :** en V1, Gmail fonctionne avec un **mot de passe d'application**, ou avec **un identifiant client OAuth que chacun crée lui-même** et saisit dans les réglages. Le bouton officiel « Se connecter avec Google » (projet vérifié par Google) viendra plus tard, si le projet décolle.

### Signature des binaires
**Décidé avec le porteur du projet :** les binaires sont publiés **sans signature** au début. Le README explique comment passer l'avertissement de Windows SmartScreen (« Exécuter quand même ») et de macOS Gatekeeper (clic droit, puis « Ouvrir »). La CI doit permettre d'ajouter la signature plus tard sans être réécrite.

Microsoft ne pose pas ce problème : client public, sans secret.

## 7. Façon de travailler (apprentissage)

- On avance par petites étapes, chacune livrant quelque chose qui tourne.
- Claude explique chaque concept Rust la première fois qu'il apparaît (propriété, emprunts, `Result`, `async`, traits…).
- Claude propose régulièrement de petits exercices réels à faire soi-même (ajouter un champ, une colonne, un test).
- Le code reste simple : pas de macros maison ni d'abstractions prématurées.

## 8. Laissé au constructeur

- Le schéma exact de la base SQLite et sa stratégie de migration.
- La stratégie précise de synchronisation (UIDVALIDITY, CONDSTORE/QRESYNC si disponibles).
- L'algorithme exact de l'anti-spam et des heuristiques de tri.
- Les détails visuels (couleurs, icônes, espacements), dans l'esprit de la maquette B.
- Le découpage précis en étapes du plan d'implémentation.

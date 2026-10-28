# Tests de bout en bout contre un vrai serveur local

`crates/avero-sync/tests/e2e_local.rs` fait passer du vrai courrier par le
code d'Avero (envoi SMTP, copie dans « Envoyés », synchronisation dans une
base, lecture du corps et des pièces jointes, lu/non lu, déplacement,
suppression définitive) contre [GreenMail](https://greenmail-mail-test.github.io/greenmail/)
(Apache-2.0), un serveur IMAP/SMTP en Java qui tourne sur la machine.

## Lancer

```powershell
powershell -ExecutionPolicy Bypass -File scripts\e2e-local.ps1
```

Il faut Java (testé avec Java 25), `openssl` (celui de Git pour Windows convient)
et cargo. Ni droits administrateur, ni Docker. Le script :

1. télécharge `greenmail-standalone-2.1.15.jar` depuis Maven Central
   (SHA-1 vérifié) dans `%TEMP%\avero-e2e\`, s'il n'y est pas déjà ;
2. y crée une autorité de certification jetable et un certificat pour
   `localhost` / `127.0.0.1` (magasin `server.p12` pour GreenMail) ;
3. démarre GreenMail en arrière-plan : IMAPS sur 3993, SMTPS sur 3465
   (API REST de GreenMail sur 3080), comptes `alice@localhost.test` et
   `bob@localhost.test` avec des mots de passe tirés au hasard à chaque
   lancement ;
4. lance `cargo test -p avero-sync --features test-ca --test e2e_local --
   --ignored --test-threads=1` ;
5. arrête GreenMail, même en cas d'échec.

Options : `-WorkDir`, `-ImapPort`, `-SmtpPort`, `-ApiPort`. Journal du
serveur : `%TEMP%\avero-e2e\greenmail.log`. Pour repartir de zéro,
supprimer ce dossier.

## Le certificat de test

Avero n'accepte que le TLS et ne fait confiance qu'aux autorités du
système. Pour que les tests acceptent le certificat de GreenMail sans
toucher au magasin de Windows, `avero-sync` a une fonctionnalité cargo
`test-ca`, désactivée par défaut et jamais activée par `avero-app` :
compilée avec elle, et seulement si la variable `AVERO_TEST_CA_PEM` désigne
un fichier PEM, l'autorité qu'il contient s'ajoute à celles du système pour
IMAP et pour SMTP (`crates/avero-sync/src/test_ca.rs`). L'application
livrée ne contient pas ce code.

## Variables lues par les tests

| Variable | Rôle | Défaut |
|---|---|---|
| `AVERO_TEST_CA_PEM` | autorité de test (PEM) | — |
| `AVERO_E2E_HOST` | hôte du serveur | `127.0.0.1` |
| `AVERO_E2E_IMAP_PORT` | IMAP, TLS implicite | `3993` |
| `AVERO_E2E_SMTP_PORT` | SMTP, TLS implicite | `3465` |
| `AVERO_E2E_ALICE`, `AVERO_E2E_ALICE_PASSWORD` | compte expéditeur | — |
| `AVERO_E2E_BOB`, `AVERO_E2E_BOB_PASSWORD` | compte destinataire | — |

Les mots de passe ne sont jamais affichés.

## Ce qui n'est pas couvert

- SMTP en STARTTLS (port 587) : GreenMail ne le propose pas ;
- le repli COPY + EXPUNGE des serveurs sans `MOVE` : GreenMail annonce
  `MOVE` et `UIDPLUS` (ce repli reste couvert par les tests unitaires) ;
- OAuth2 (voir `tests/live_refusals.rs` pour les refus réels de Gmail et
  Outlook).

# Configurer la connexion OAuth (Google et Microsoft)

Avero connecte les comptes Gmail et Outlook par le navigateur (OAuth2, flux
« application de bureau » avec redirection locale et PKCE). Pour cela, le
responsable du projet doit enregistrer Avero auprès de Google et de Microsoft
et fournir les identifiants client obtenus.

Ces identifiants ne sont pas des secrets : pour une application de bureau, ils
sont de toute façon embarqués dans le programme. Les jetons des utilisateurs,
eux, ne quittent jamais la machine (le jeton de rafraîchissement va dans le
trousseau du système).

Sans identifiant configuré, le bouton correspondant reste indisponible et
indique pourquoi ; le mot de passe d'application reste possible pour Gmail.

## Où mettre les identifiants

Avero cherche les identifiants dans cet ordre :

1. **Au lancement**, dans le fichier `oauth.json` du dossier de configuration :

   ```json
   {
     "google": {
       "client_id": "1234567890-abc.apps.googleusercontent.com",
       "client_secret": "GOCSPX-..."
     },
     "microsoft": {
       "client_id": "00000000-0000-0000-0000-000000000000"
     }
   }
   ```

   Emplacement du dossier de configuration :

   | Système | Chemin |
   |---|---|
   | Windows | `%APPDATA%\Avero\Avero\config\oauth.json` |
   | macOS | `~/Library/Application Support/org.Avero.Avero/oauth.json` |
   | Linux | `$XDG_CONFIG_HOME/avero/oauth.json` (par défaut `~/.config/avero/oauth.json`) |

2. **À la compilation**, par variables d'environnement :

   ```sh
   AVERO_GOOGLE_CLIENT_ID=... \
   AVERO_GOOGLE_CLIENT_SECRET=... \
   AVERO_MICROSOFT_CLIENT_ID=... \
   cargo build --release
   ```

   Sous PowerShell : `$env:AVERO_GOOGLE_CLIENT_ID = "..."` avant `cargo build`.

Une valeur vide compte comme absente. Le fichier `oauth.json` l'emporte sur les
valeurs compilées, fournisseur par fournisseur.

## Google (Gmail)

1. Ouvrir la [console Google Cloud](https://console.cloud.google.com/) et
   **créer un projet** (par exemple « Avero »).
2. **API et services → Écran de consentement OAuth** (« Google Auth Platform ») :
   - type d'utilisateur : **Externe** ;
   - nom de l'application « Avero », adresse d'assistance, logo facultatif ;
   - **Accès aux données / champs d'application** : ajouter
     `https://mail.google.com/` (ainsi que `openid` et `email`, non sensibles) ;
   - **Audience → Utilisateurs test** : ajouter les adresses Gmail qui pourront
     se connecter tant que l'application n'est pas validée.
3. **Identifiants (Clients) → Créer un ID client OAuth** :
   - type d'application : **Application de bureau** ;
   - nom : « Avero desktop ».

   Google affiche un **ID client** et un **code secret du client**. Pour une
   application de bureau, ce « secret » n'est pas confidentiel ; Google l'exige
   néanmoins lors de l'échange du code. Les deux vont dans `oauth.json` ou dans
   `AVERO_GOOGLE_CLIENT_ID` / `AVERO_GOOGLE_CLIENT_SECRET`.

Aucune URI de redirection n'est à déclarer : les clients « Application de
bureau » acceptent automatiquement `http://127.0.0.1:<port>`.

### Limites tant que l'application n'est pas validée

- Seuls les **utilisateurs test** déclarés peuvent se connecter, **100 au
  maximum** sur toute la vie du projet.
- Chaque utilisateur voit un avertissement « Google n'a pas validé cette
  application » qu'il doit passer (« Paramètres avancés → Accéder à Avero »).
- En mode « Test », les jetons de rafraîchissement expirent au bout de 7 jours :
  il faut alors se reconnecter.

### Publication publique

`https://mail.google.com/` est un champ d'application **restreint**. Pour
ouvrir Avero à tous, il faut passer l'écran de consentement « En production »
et demander la **validation Google** : vérification du domaine et de la
politique de confidentialité, vidéo de démonstration, et pour les champs
restreints une **évaluation de sécurité** par un organisme agréé (CASA),
renouvelée chaque année. Prévoir plusieurs semaines.

## Microsoft (Outlook, Hotmail, Microsoft 365)

1. Ouvrir le [centre d'administration Microsoft Entra](https://entra.microsoft.com/)
   (ou le portail Azure) → **Applications → Inscriptions d'applications →
   Nouvelle inscription**.
2. Nom « Avero » ; **types de comptes pris en charge** : « Comptes dans un
   annuaire organisationnel (tout locataire Microsoft Entra ID) **et comptes
   Microsoft personnels** (Skype, Xbox, Outlook.com…) ».
3. Ne rien mettre dans l'URI de redirection pour l'instant, puis **Inscrire**.
   Noter l'**ID d'application (client)** : c'est `AVERO_MICROSOFT_CLIENT_ID`.
4. **Authentification → Ajouter une plateforme → Applications mobiles et de
   bureau** : URI de redirection personnalisée `http://localhost`
   (Microsoft accepte alors n'importe quel port sur `localhost`).
5. Toujours dans **Authentification**, paramètres avancés : activer
   **Autoriser les flux de clients publics** (« Allow public client flows »).
   Aucun secret client n'est nécessaire.
6. **Autorisations d'API → Ajouter une autorisation** :
   - **API utilisées par mon organisation → Office 365 Exchange Online** →
     **Autorisations déléguées** : `IMAP.AccessAsUser.All` et `SMTP.Send` ;
   - **Microsoft Graph → Autorisations déléguées** : `offline_access`,
     `openid` et `email`.

   Aucun consentement administrateur n'est requis pour ces autorisations
   déléguées : chaque utilisateur consent lui-même à la première connexion.

Remarque pour les comptes professionnels : l'administrateur du locataire doit
laisser IMAP et SMTP authentifié activés pour la boîte aux lettres, et peut
restreindre le consentement des utilisateurs aux applications tierces.

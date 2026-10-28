//! The one-time assistant that registers Avero with Google or Microsoft.
//!
//! Google and Microsoft only let an app sign in with an OAuth client its
//! owner created in their console: Avero cannot ship one. The assistant
//! lists the steps with a button to the exact console page for each, then
//! takes the id (and Google's secret) and saves them in `oauth.json`.
//! After that, "Continuer avec Google" is one click for every account.

use avero_oauth::{ClientConfig, Provider};
use std::fmt;

/// Google Cloud console pages, in the order of the steps.
pub const GOOGLE_PROJECT_CREATE: &str = "https://console.cloud.google.com/projectcreate";
pub const GOOGLE_GMAIL_API: &str =
    "https://console.cloud.google.com/apis/library/gmail.googleapis.com";
pub const GOOGLE_AUTH_OVERVIEW: &str = "https://console.cloud.google.com/auth/overview";
pub const GOOGLE_AUTH_AUDIENCE: &str = "https://console.cloud.google.com/auth/audience";
pub const GOOGLE_AUTH_SCOPES: &str = "https://console.cloud.google.com/auth/scopes";
pub const GOOGLE_CLIENT_CREATE: &str = "https://console.cloud.google.com/auth/clients/create";
/// The scope to add on the "Accès aux données" page.
pub const GOOGLE_SCOPE: &str = "https://mail.google.com/";

/// Microsoft Entra pages.
pub const MICROSOFT_REGISTER: &str =
    "https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/CreateApplicationBlade";
pub const MICROSOFT_APP_LIST: &str =
    "https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade";
/// The redirect URI to declare ("Client public/natif").
pub const MICROSOFT_REDIRECT: &str = "http://localhost";

/// A button of a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Opens a console page in the browser.
    Open(&'static str, String),
    /// Copies a value the user must paste in the console.
    Copy(&'static str, &'static str),
}

/// One numbered step of the assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub title: &'static str,
    pub detail: &'static str,
    pub actions: Vec<Action>,
}

/// The steps for `provider`. Microsoft's later steps link to the
/// application's own pages once its id has been pasted.
pub fn steps(provider: Provider, client_id: &str) -> Vec<Step> {
    let open = |label, url: &str| Action::Open(label, url.to_string());
    match provider {
        Provider::Google => vec![
            Step {
                title: "Créez un projet",
                detail: "Connectez-vous avec votre compte Google et nommez le projet « Avero ».",
                actions: vec![open("Ouvrir", GOOGLE_PROJECT_CREATE)],
            },
            Step {
                title: "Activez l'API Gmail",
                detail: "Cliquez sur « Activer ».",
                actions: vec![open("Ouvrir", GOOGLE_GMAIL_API)],
            },
            Step {
                title: "Configurez l'écran de consentement",
                detail: "« Commencer » : nom « Avero », votre adresse, audience « Externe ». \
                         Puis, dans Audience, ajoutez votre adresse Gmail comme utilisateur test.",
                actions: vec![
                    open("Commencer", GOOGLE_AUTH_OVERVIEW),
                    open("Audience", GOOGLE_AUTH_AUDIENCE),
                ],
            },
            Step {
                title: "Autorisez l'accès à Gmail",
                detail: "« Ajouter ou supprimer des champs d'application », collez l'adresse \
                         copiée dans « Ajouter manuellement », puis enregistrez.",
                actions: vec![
                    Action::Copy("Copier le champ", GOOGLE_SCOPE),
                    open("Ouvrir", GOOGLE_AUTH_SCOPES),
                ],
            },
            Step {
                title: "Créez l'ID client",
                detail: "Type « Application de bureau », nom « Avero ». Google affiche alors \
                         l'ID client et le code secret : collez-les à droite.",
                actions: vec![open("Ouvrir", GOOGLE_CLIENT_CREATE)],
            },
        ],
        Provider::Microsoft => {
            let app_page = |blade: &str| match avero_oauth::check_client_id(provider, client_id) {
                Ok(id) => format!(
                    "https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/\
                     ApplicationMenuBlade/~/{blade}/appId/{id}"
                ),
                Err(_) => MICROSOFT_APP_LIST.to_string(),
            };
            vec![
                Step {
                    title: "Inscrivez l'application",
                    detail: "Nom « Avero » ; comptes : « tout annuaire organisationnel et comptes \
                             Microsoft personnels » ; redirection : « Client public/natif » avec \
                             l'adresse copiée. Collez ensuite l'ID d'application à droite.",
                    actions: vec![
                        Action::Copy("Copier l'adresse", MICROSOFT_REDIRECT),
                        open("Ouvrir", MICROSOFT_REGISTER),
                    ],
                },
                Step {
                    title: "Autorisez les clients publics",
                    detail: "Authentification : « Autoriser les flux de clients publics » sur Oui.",
                    actions: vec![Action::Open("Ouvrir", app_page("Authentication"))],
                },
                Step {
                    title: "Ajoutez les autorisations",
                    detail: "Autorisations d'API : Office 365 Exchange Online → \
                             IMAP.AccessAsUser.All et SMTP.Send ; Microsoft Graph → \
                             offline_access.",
                    actions: vec![Action::Open("Ouvrir", app_page("CallAnAPI"))],
                },
            ]
        }
    }
}

/// What the user pasted. `Debug` hides the secret.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SetupFields {
    pub client_id: String,
    pub client_secret: String,
}

impl fmt::Debug for SetupFields {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SetupFields")
            .field("client_id", &self.client_id)
            .field("client_secret", &"***")
            .finish()
    }
}

impl SetupFields {
    /// Prefilled with an existing client (changing it in the settings).
    pub fn from_client(client: Option<ClientConfig>) -> SetupFields {
        match client {
            Some(client) => SetupFields {
                client_id: client.client_id,
                client_secret: client.client_secret.unwrap_or_default(),
            },
            None => SetupFields::default(),
        }
    }

    /// The problem with the typed id, once something is typed.
    pub fn id_error(&self, provider: Provider) -> Option<&'static str> {
        if self.client_id.trim().is_empty() {
            return None;
        }
        avero_oauth::check_client_id(provider, &self.client_id).err()
    }

    /// The problem with the typed secret, once something is typed.
    pub fn secret_error(&self) -> Option<&'static str> {
        if self.client_secret.trim().is_empty() {
            return None;
        }
        avero_oauth::check_client_secret(&self.client_secret).err()
    }

    /// Whether the id (and, for Google, the secret) look right.
    pub fn is_complete(&self, provider: Provider) -> bool {
        self.client(provider).is_ok()
    }

    /// The client to save, or the first problem.
    pub fn client(&self, provider: Provider) -> Result<ClientConfig, &'static str> {
        let client_id = avero_oauth::check_client_id(provider, &self.client_id)?;
        let client_secret = match provider {
            Provider::Google => Some(avero_oauth::check_client_secret(&self.client_secret)?),
            Provider::Microsoft => None,
        };
        Ok(ClientConfig {
            client_id,
            client_secret,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOGLE_ID: &str = "1234567890-abcdef.apps.googleusercontent.com";
    const MICROSOFT_ID: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

    #[test]
    fn google_steps_open_the_console_pages() {
        let urls: Vec<String> = steps(Provider::Google, "")
            .into_iter()
            .flat_map(|s| s.actions)
            .filter_map(|a| match a {
                Action::Open(_, url) => Some(url),
                Action::Copy(..) => None,
            })
            .collect();
        assert_eq!(
            urls,
            [
                GOOGLE_PROJECT_CREATE,
                GOOGLE_GMAIL_API,
                GOOGLE_AUTH_OVERVIEW,
                GOOGLE_AUTH_AUDIENCE,
                GOOGLE_AUTH_SCOPES,
                GOOGLE_CLIENT_CREATE,
            ]
        );
        assert!(
            steps(Provider::Google, "")
                .iter()
                .flat_map(|s| &s.actions)
                .any(|a| *a == Action::Copy("Copier le champ", GOOGLE_SCOPE))
        );
    }

    #[test]
    fn microsoft_steps_link_to_the_app_once_its_id_is_known() {
        let before = steps(Provider::Microsoft, "");
        assert_eq!(
            before[1].actions,
            [Action::Open("Ouvrir", MICROSOFT_APP_LIST.to_string())]
        );
        let after = steps(Provider::Microsoft, &format!(" {MICROSOFT_ID} "));
        let Action::Open(_, url) = &after[2].actions[0] else {
            panic!("open action expected");
        };
        assert!(
            url.ends_with(&format!("/CallAnAPI/appId/{MICROSOFT_ID}")),
            "{url}"
        );
        assert!(!url.contains(' '));
    }

    #[test]
    fn inline_validation_waits_for_input() {
        let mut fields = SetupFields::default();
        assert_eq!(fields.id_error(Provider::Google), None);
        assert_eq!(fields.secret_error(), None);
        assert!(!fields.is_complete(Provider::Google));
        fields.client_id = "1234".into();
        assert!(fields.id_error(Provider::Google).is_some());
        fields.client_id = GOOGLE_ID.into();
        assert_eq!(fields.id_error(Provider::Google), None);
        assert_eq!(
            fields.client(Provider::Google),
            Err("Collez le code secret du client")
        );
        fields.client_secret = " GOCSPX-abcdefghijkl ".into();
        assert_eq!(
            fields.client(Provider::Google),
            Ok(ClientConfig {
                client_id: GOOGLE_ID.into(),
                client_secret: Some("GOCSPX-abcdefghijkl".into()),
            })
        );
    }

    #[test]
    fn microsoft_needs_no_secret() {
        let fields = SetupFields {
            client_id: MICROSOFT_ID.into(),
            client_secret: String::new(),
        };
        assert_eq!(
            fields.client(Provider::Microsoft).map(|c| c.client_secret),
            Ok(None)
        );
        assert!(fields.id_error(Provider::Google).is_some());
    }

    #[test]
    fn debug_hides_the_secret() {
        let fields = SetupFields {
            client_id: GOOGLE_ID.into(),
            client_secret: "GOCSPX-very-secret".into(),
        };
        let debug = format!("{fields:?}");
        assert!(!debug.contains("very-secret"), "{debug}");
        assert!(debug.contains(GOOGLE_ID));
    }
}

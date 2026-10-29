//! Demo mode (`AVERO_DEMO=1`): an in-memory database with three made-up
//! accounts and about forty French messages, a few of them answers that
//! make conversations, some newsletters and notifications (for the tabs
//! and "Se désinscrire"), a few starred ones, a few rules, spam (one of
//! them caught by the local filter), two conversations put away for
//! later, a draft and a mail scheduled for tomorrow, two spaces ("Perso"
//! and "Pro"), and no network at all: "Chercher aussi sur le serveur"
//! makes up an older message (see `server_hits`).
//! `AVERO_DEMO=empty` starts with no account (welcome screen);
//! `AVERO_DEMO=google-setup` (or `microsoft-setup`) also opens the
//! connection assistant, as after a click on "Continuer avec Google".

use crate::later::{self, When};
use avero_core::rules::{Condition, RuleDef, RuleField, RuleMove, block_rule};
use avero_core::store::Store;
use avero_core::{
    AccountFilter, AccountId, AuthKind, DraftFields, FolderRole, Listing, MessageHeaders,
    NewAccount, NewMessage, Security, ServerConfig,
};
use avero_sync::search::ServerHits;
use chrono::{Local, TimeZone};

/// What `AVERO_DEMO` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoKind {
    Seeded,
    Empty,
    /// No account, the OAuth assistant open.
    Setup(avero_oauth::Provider),
}

/// Reads the value of `AVERO_DEMO`.
pub fn from_env_value(value: Option<&str>) -> Option<DemoKind> {
    match value.map(str::trim) {
        None | Some("") | Some("0") => None,
        Some(v) if v.eq_ignore_ascii_case("empty") => Some(DemoKind::Empty),
        Some(v) if v.eq_ignore_ascii_case("google-setup") => {
            Some(DemoKind::Setup(avero_oauth::Provider::Google))
        }
        Some(v) if v.eq_ignore_ascii_case("microsoft-setup") => {
            Some(DemoKind::Setup(avero_oauth::Provider::Microsoft))
        }
        Some(_) => Some(DemoKind::Seeded),
    }
}

/// One made-up message.
struct Sample {
    account: usize,
    role: FolderRole,
    from: (&'static str, &'static str),
    subject: &'static str,
    body: &'static str,
    minutes_ago: i64,
    seen: bool,
    attachment: Option<(&'static str, &'static str, usize)>,
}

const ACCOUNTS: [(&str, &str, &str, AuthKind); 3] = [
    (
        "Perso",
        "camille.martin@gmail.com",
        "gmail.com",
        AuthKind::OAuthGoogle,
    ),
    (
        "Travail",
        "camille@atelier-lumen.fr",
        "atelier-lumen.fr",
        AuthKind::Password,
    ),
    (
        "Asso",
        "tresorerie@velo-club-lyon.org",
        "velo-club-lyon.org",
        AuthKind::Password,
    ),
];

/// Signature of the "Travail" demo account (the others have none).
const WORK_SIGNATURE: &str = "Camille Martin\nAtelier Lumen · 04 78 00 00 00";

const H: i64 = 60;
const D: i64 = 24 * 60;

#[rustfmt::skip]
fn samples() -> Vec<Sample> {
    use FolderRole::{Archive, Inbox, Junk, Sent};
    let s = |account, role, from, subject, body, minutes_ago, seen, attachment| Sample {
        account, role, from, subject, body, minutes_ago, seen, attachment,
    };
    vec![
        s(1, Inbox, ("Léa Bernard", "lea.bernard@atelier-lumen.fr"), "Maquettes v3 pour le client Orsay",
          "Salut Camille,\n\nVoici la v3 des maquettes avec les retours de lundi : la page d'accueil est plus aérée et j'ai repris la palette.\n\nTu peux jeter un œil avant 16 h ? Je dois l'envoyer au client ce soir.\n\nMerci !\nLéa",
          12, false, Some(("Orsay-maquettes-v3.pdf", "application/pdf", 2_480_000))),
        s(0, Inbox, ("SNCF Connect", "noreply@sncf-connect.com"), "Votre e-billet Lyon → Paris du 7 novembre",
          "Bonjour Camille,\n\nVotre voyage est confirmé.\n\nAller : Lyon Part-Dieu 08:04 → Paris Gare de Lyon 10:00\nVoiture 14, place 82 (fenêtre)\n\nVotre e-billet est joint à ce message. Bon voyage !",
          38, false, Some(("e-billet-LYS-PAR.pdf", "application/pdf", 184_000))),
        s(2, Inbox, ("Marc Fontaine", "marc.fontaine@velo-club-lyon.org"), "Sortie de dimanche : on part à 8 h 30",
          "Bonjour à tous,\n\nLa sortie des Monts d'Or est maintenue dimanche. Rendez-vous à 8 h 30 devant le local, 72 km et 900 m de dénivelé.\n\nPensez à vos chambres à air de rechange !\n\nMarc",
          55, false, None),
        s(1, Inbox, ("Julien Roche", "julien.roche@atelier-lumen.fr"), "Point budget T4 — à préparer pour jeudi",
          "Camille,\n\nPour le point de jeudi, peux-tu préparer le prévisionnel du T4 avec les trois scénarios dont on a parlé ?\n\nUn tableau simple suffit, on affinera ensemble.\n\nJulien",
          2 * H, false, None),
        s(0, Inbox, ("Maman", "francoise.martin@orange.fr"), "Repas de dimanche",
          "Coucou ma chérie,\n\nTu viens toujours dimanche midi ? Ton frère apporte le dessert. Dis-moi si tu veux que je fasse le gratin.\n\nGros bisous\nMaman",
          3 * H, false, None),
        s(1, Inbox, ("Notion", "notify@makenotion.com"), "Sophie a commenté « Roadmap 2027 »",
          "Sophie Lambert a commenté la page Roadmap 2027 :\n\n« Je propose de décaler la refonte du site au T2, le T1 est déjà chargé. »\n\nRépondez directement depuis Notion.",
          4 * H, true, None),
        s(2, Inbox, ("HelloAsso", "contact@helloasso.com"), "3 nouvelles adhésions au Vélo Club",
          "Bonne nouvelle !\n\nTrois nouvelles personnes ont adhéré au Vélo Club de Lyon cette semaine. Le détail des paiements est disponible dans votre espace.\n\nL'équipe HelloAsso",
          5 * H, false, Some(("adhesions-octobre.csv", "text/csv", 3_200))),
        s(0, Inbox, ("Banque Rhône-Alpes", "info@banque-ra.fr"), "Votre relevé de compte d'octobre est disponible",
          "Bonjour,\n\nVotre relevé de compte du mois d'octobre est disponible dans votre espace client.\n\nPar sécurité, nous ne vous demanderons jamais vos codes par e-mail.",
          7 * H, true, None),
        s(1, Inbox, ("Sophie Lambert", "sophie.lambert@atelier-lumen.fr"), "Re: Atelier design system",
          "Top, merci pour le compte rendu !\n\nJe bloque le créneau du 12 pour la deuxième session. On pourrait inviter l'équipe dev cette fois ?\n\nSophie",
          9 * H, true, None),
        s(0, Inbox, ("Thomas Petit", "thomas.petit@gmail.com"), "Photos du week-end à Annecy 📸",
          "Hello !\n\nJ'ai enfin trié les photos du week-end. Il y en a une de toi sur le paddle qui est géniale.\n\nLe lien de l'album expire dans 7 jours.\n\nThomas",
          D + 2 * H, true, None),
        s(2, Inbox, ("Mairie de Lyon", "associations@mairie-lyon.fr"), "Subvention 2027 : dossier à déposer avant le 30 novembre",
          "Madame la Trésorière,\n\nLe dépôt des demandes de subvention pour 2027 est ouvert jusqu'au 30 novembre. Le formulaire et la liste des pièces justificatives sont joints.\n\nLe service Vie associative",
          D + 4 * H, false, Some(("formulaire-subvention-2027.pdf", "application/pdf", 640_000))),
        s(1, Inbox, ("Figma", "no-reply@figma.com"), "Léa vous a invité à modifier « Orsay — site »",
          "Léa Bernard vous a invité à modifier le fichier « Orsay — site ».\n\nOuvrir dans Figma.",
          D + 5 * H, true, None),
        s(0, Inbox, ("Doctolib", "no-reply@doctolib.fr"), "Rappel : rendez-vous chez le Dr Morel jeudi à 9 h 15",
          "Bonjour Camille,\n\nNous vous rappelons votre rendez-vous :\nDr Antoine Morel, médecin généraliste\nJeudi 30 octobre à 9 h 15\n12 rue Victor Hugo, Lyon 2e\n\nEn cas d'empêchement, pensez à annuler.",
          D + 8 * H, true, None),
        s(1, Inbox, ("Julien Roche", "julien.roche@atelier-lumen.fr"), "Contrat Orsay signé 🎉",
          "Bonne nouvelle : le contrat Orsay est signé ! Bravo à toute l'équipe.\n\nOn fête ça vendredi à 18 h.\n\nJulien",
          2 * D, true, Some(("contrat-orsay-signe.pdf", "application/pdf", 1_120_000))),
        s(2, Inbox, ("Claire Dubois", "claire.dubois@free.fr"), "Remboursement des maillots",
          "Bonjour Camille,\n\nJ'ai avancé 240 € pour la commande des maillots. Je te transmets la facture, peux-tu me faire le virement quand tu as un moment ?\n\nMerci, Claire",
          2 * D + 3 * H, true, Some(("facture-maillots.pdf", "application/pdf", 96_000))),
        s(0, Inbox, ("Le Monde", "newsletter@lemonde.fr"), "La lettre du matin : ce qu'il faut retenir",
          "<table width=\"100%\"><tr><td><img src=\"https://example.org/logo.png\" alt=\"Logo\"></td>\
<td align=\"right\"><a href=\"https://example.org/lettre\">Voir dans le navigateur</a></td></tr>\
<tr><td colspan=\"2\"><h1>La lettre du matin</h1><p>Bonjour,</p><p>Au sommaire ce matin : <strong>climat</strong>, \
budget 2027 et un portrait de la nouvelle génération de cyclistes urbains.</p>\
<p><a href=\"https://example.org/climat\">Climat : ce que change l'accord de la nuit</a><br>\
<a href=\"https://example.org/budget\">Budget 2027 : les trois chiffres à retenir</a></p><p>Bonne lecture.</p>\
<p><small>Vous recevez cette lettre car vous y êtes inscrite. \
<a href=\"https://example.org/desinscription\">Se désinscrire</a></small></p></td></tr></table>",
          2 * D + 10 * H, true, None),
        s(1, Inbox, ("Malt", "team@malt.fr"), "Nouvelle mission : refonte d'une application mobile",
          "Une mission correspondant à votre profil vient d'être publiée : refonte UX d'une application de covoiturage, 20 jours, Lyon ou télétravail.",
          3 * D, true, None),
        s(0, Inbox, ("Thomas Petit", "thomas.petit@gmail.com"), "Ciné jeudi ?",
          "Ça te dit d'aller voir le nouveau film de Céline Sciamma jeudi soir ? Séance à 20 h 30 au Comoedia.",
          3 * D + 5 * H, true, None),
        s(2, Inbox, ("Marc Fontaine", "marc.fontaine@velo-club-lyon.org"), "Assemblée générale : ordre du jour",
          "Bonjour,\n\nVoici l'ordre du jour de l'AG du 22 novembre :\n1. Rapport moral\n2. Rapport financier (Camille)\n3. Calendrier 2027\n4. Questions diverses\n\nMarc",
          4 * D, true, Some(("ordre-du-jour-AG.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", 28_000))),
        s(1, Inbox, ("Pauline Garnier", "pauline@studio-garnier.fr"), "Proposition de collaboration",
          "Bonjour Camille,\n\nJ'ai découvert le travail de l'Atelier Lumen sur le projet du musée des Confluences. Seriez-vous disponible pour un échange la semaine prochaine ?\n\nBien cordialement,\nPauline Garnier",
          4 * D + 6 * H, false, None),
        s(0, Inbox, ("Free", "facture@free-mobile.fr"), "Votre facture Free Mobile est disponible",
          "Bonjour,\n\nVotre facture du mois d'octobre d'un montant de 15,99 € est disponible dans votre espace abonné.",
          5 * D, true, None),
        s(1, Inbox, ("GitHub", "noreply@github.com"), "[atelier-lumen/site] Pull request #128 fusionnée",
          "La pull request #128 « Nouvelle page équipe » a été fusionnée dans main par julien-roche.",
          5 * D + 3 * H, true, None),
        s(2, Inbox, ("Fédération française de cyclisme", "licences@ffc.fr"), "Renouvellement des licences 2027",
          "Madame, Monsieur,\n\nLa campagne de renouvellement des licences 2027 débutera le 1er décembre. Vous trouverez ci-joint la note d'information.",
          6 * D, true, Some(("note-licences-2027.pdf", "application/pdf", 310_000))),
        s(0, Inbox, ("Airbnb", "automated@airbnb.com"), "Votre réservation à Annecy est confirmée",
          "Réservation confirmée : studio avec vue sur le lac, du 14 au 16 novembre. L'hôte vous enverra les instructions d'arrivée 48 h avant.",
          7 * D, true, None),
        s(1, Inbox, ("Sophie Lambert", "sophie.lambert@atelier-lumen.fr"), "Atelier design system",
          "Coucou,\n\nJe propose qu'on fasse un premier atelier design system mardi prochain. 2 h, salle Rhône. Ça te va ?\n\nSophie",
          8 * D, true, None),
        s(0, Inbox, ("Lucie Martin", "lucie.martin@gmail.com"), "Anniversaire de Papa 🎂",
          "Hello sœurette,\n\nOn se cotise pour le cadeau de Papa ? Je pensais à un vélo électrique, il en parle depuis des mois. Ça ferait 120 € chacun.\n\nLucie",
          9 * D, true, None),
        s(2, Inbox, ("Décathlon Pro", "pro@decathlon.fr"), "Devis n° 2026-1184 — kits de réparation",
          "Bonjour,\n\nVeuillez trouver ci-joint le devis demandé pour 15 kits de réparation et 10 pompes.\n\nCordialement,\nService Pro",
          10 * D, true, Some(("devis-2026-1184.pdf", "application/pdf", 154_000))),
        s(1, Inbox, ("Julien Roche", "julien.roche@atelier-lumen.fr"), "Congés de fin d'année",
          "Pour info, l'atelier sera fermé du 24 décembre au 2 janvier. Pensez à poser vos jours dans l'outil RH avant le 15 novembre.",
          11 * D, true, None),
        s(0, Inbox, ("Spotify", "no-reply@spotify.com"), "Votre récap de la semaine",
          "Cette semaine, vous avez écouté 9 h de musique. Votre titre le plus écouté : « La Grenade » de Clara Luciani.",
          12 * D, true, None),
        s(1, Inbox, ("Antoine Leroy", "antoine.leroy@musee-orsay.fr"), "Retours sur la première maquette",
          "Bonjour Camille,\n\nMerci pour cette première version. Nous aimons beaucoup la typographie. Deux points : la navigation pourrait être plus visible, et nous aimerions une version en anglais.\n\nBien à vous,\nAntoine Leroy",
          13 * D, true, None),
        s(2, Inbox, ("Claire Dubois", "claire.dubois@free.fr"), "Covoiturage pour la course de Bourg",
          "Qui a de la place dans sa voiture pour la course de Bourg-en-Bresse le 9 ? Je peux prendre trois personnes et deux vélos.",
          14 * D, true, None),
        s(0, Inbox, ("Thomas Petit", "thomas.petit@gmail.com"), "Week-end à Annecy",
          "On part vendredi soir ou samedi matin ? Je peux conduire. Il faut qu'on réserve le paddle avant mercredi.",
          16 * D, true, None),
        s(1, Inbox, ("Léa Bernard", "lea.bernard@atelier-lumen.fr"), "Moodboard Orsay",
          "Voici le moodboard pour Orsay, j'ai mis l'accent sur les tons pierre et les grandes photos.",
          18 * D, true, Some(("moodboard-orsay.png", "image/png", 3_400_000))),
        s(0, Inbox, ("La Poste", "suivi@laposte.fr"), "Votre colis a été livré",
          "Votre colis n° 6A123456789 a été livré dans votre boîte aux lettres le 9 octobre à 11 h 42.",
          19 * D, true, None),
        s(0, Sent, ("Camille Martin", "camille.martin@gmail.com"), "Re: Repas de dimanche",
          "Oui je viens ! Fais le gratin, s'il te plaît 😊\n\nÀ dimanche,\nCamille",
          2 * H, true, None),
        s(1, Sent, ("Camille Martin", "camille@atelier-lumen.fr"), "Compte rendu — atelier design system",
          "Bonjour à toutes et à tous,\n\nVoici le compte rendu de l'atelier de mardi. Prochaine session le 12.\n\nCamille",
          9 * D, true, Some(("compte-rendu-atelier.pdf", "application/pdf", 210_000))),
        s(2, Sent, ("Camille Martin", "tresorerie@velo-club-lyon.org"), "Re: Remboursement des maillots",
          "C'est fait, le virement de 240 € part aujourd'hui. Merci Claire !",
          2 * D, true, None),
        s(1, Archive, ("Julien Roche", "julien.roche@atelier-lumen.fr"), "Bienvenue à l'Atelier Lumen",
          "Bienvenue Camille ! Ton poste est prêt, on se retrouve lundi à 9 h pour le café d'accueil.",
          400 * D, true, None),
        s(0, Archive, ("Impots.gouv", "ne-pas-repondre@dgfip.finances.gouv.fr"), "Votre avis d'impôt est disponible",
          "Votre avis d'impôt sur les revenus est disponible dans votre espace particulier.",
          60 * D, true, None),
        s(2, Inbox, ("Marc Fontaine", "marc.fontaine@velo-club-lyon.org"), "Merci pour la trésorerie !",
          "Camille, merci encore pour le travail sur les comptes, l'expert-comptable était ravi.",
          20 * D, true, None),
        s(1, Sent, ("Camille Martin", "camille@atelier-lumen.fr"), "Re: Point budget T4 — à préparer pour jeudi",
          "Bien reçu, je te prépare ça pour mercredi soir. Tu veux aussi le détail par client ?\n\nCamille",
          90, true, None),
        s(1, Inbox, ("Julien Roche", "julien.roche@atelier-lumen.fr"), "Re: Point budget T4 — à préparer pour jeudi",
          "Oui, le détail par client serait parfait. Merci !\n\nJulien",
          25, false, None),
        s(0, Inbox, ("Maman", "francoise.martin@orange.fr"), "Re: Repas de dimanche",
          "Parfait, je fais le gratin ! Ton frère demande si tu peux apporter du pain.\n\nBisous\nMaman",
          70, false, None),
        s(2, Inbox, ("Vélo Mag", "lettre@velomag.fr"), "Les 10 plus beaux cols des Alpes",
          "Cette semaine dans Vélo Mag : les dix cols à grimper au moins une fois, l'entretien de la chaîne en hiver et nos tests de lampes.\n\nBonne route !",
          6 * H, false, None),
        s(1, Inbox, ("Behance", "newsletter@behance.net"), "Les projets de la semaine",
          "Découvrez la sélection de la semaine : identités visuelles, typographies et illustrations.",
          D + H, false, None),
        // More letters, for "Grand nettoyage".
        s(0, Inbox, ("Le Monde", "newsletter@lemonde.fr"), "La lettre du matin : la COP en questions",
          "Bonjour,\n\nAu sommaire : les négociations climatiques, le budget de la culture et nos recommandations de lecture.",
          4 * D + 10 * H, true, None),
        s(0, Inbox, ("Le Monde", "newsletter@lemonde.fr"), "La lettre du matin : l'essentiel de la semaine",
          "Bonjour,\n\nCe qu'il faut retenir de la semaine, en cinq minutes de lecture.",
          6 * D + 10 * H, true, None),
        s(0, Archive, ("Le Monde", "newsletter@lemonde.fr"), "La lettre du matin : les élections européennes",
          "Bonjour,\n\nTout ce qu'il faut savoir avant le vote de dimanche.",
          30 * D, true, None),
        s(1, Archive, ("Behance", "newsletter@behance.net"), "Les projets de la semaine : typographie",
          "Une sélection consacrée aux caractères dessinés cette année.",
          9 * D + 2 * H, true, None),
        s(2, Inbox, ("Vélo Mag", "lettre@velomag.fr"), "Bien choisir ses pneus d'hiver",
          "Pression, largeur, crampons : notre guide pour rouler serein jusqu'au printemps.",
          8 * D + 6 * H, true, None),
        // The spam folders. The newest one stands for a mail the local
        // filter caught after a sync.
        s(0, Junk, ("Gagnant Express", "promo@gagnant-express.biz"), "Félicitations ! Vous avez gagné un iPhone 17",
          "Cliquez ici pour réclamer votre cadeau avant minuit. Offre exclusive réservée aux gagnants.",
          20, false, None),
        s(1, Junk, ("Crypto Boost", "invest@crypto-boost.top"), "Doublez votre capital en 7 jours",
          "Notre algorithme garantit des gains exceptionnels. Inscrivez-vous gratuitement dès aujourd'hui.",
          8 * H, false, None),
        s(0, Junk, ("Pharmacie Discount", "ventes@pharma-discount.top"), "Prix imbattables, livraison discrète",
          "Profitez de remises exceptionnelles sur toute notre gamme. Offre limitée.",
          2 * D + 7 * H, true, None),
    ]
}

/// The list headers of a demo sender: `(List-Unsubscribe,
/// List-Unsubscribe-Post, List-Id)`. Newsletters get one way or another
/// to unsubscribe (one-click, mail only, web page only).
fn list_headers(
    from: &str,
) -> (
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
) {
    let one_click = Some("List-Unsubscribe=One-Click");
    match from {
        "newsletter@lemonde.fr" => (
            Some(
                "<mailto:desinscription@lemonde.fr?subject=unsubscribe>, <https://lemonde.fr/desinscription?u=demo>",
            ),
            one_click,
            Some("La lettre du matin <matin.lemonde.fr>"),
        ),
        "no-reply@spotify.com" => (
            Some("<https://spotify.com/unsubscribe?u=demo>"),
            one_click,
            None,
        ),
        "lettre@velomag.fr" => (
            Some("<mailto:desabonnement@velomag.fr?subject=STOP>"),
            None,
            Some("<lettre.velomag.fr>"),
        ),
        "team@malt.fr" | "newsletter@behance.net" => {
            (Some("<https://example.org/preferences?u=demo>"), None, None)
        }
        "contact@helloasso.com" => (None, None, Some("<asso.helloasso.com>")),
        _ => (None, None, None),
    }
}

/// The demo messages shown in "Favoris".
const STARRED: [&str; 4] = [
    "Maquettes v3 pour le client Orsay",
    "Subvention 2027 : dossier à déposer avant le 30 novembre",
    "Contrat Orsay signé 🎉",
    "Bienvenue à l'Atelier Lumen",
];

/// The message a sample answers: for "Re: X", the closest older sample
/// called "X" or "Re: X". Only the demo threads by subject; real mail is
/// threaded by its headers (see `Store::insert_messages`).
fn parent_of(samples: &[Sample], index: usize) -> Option<usize> {
    let sample = &samples[index];
    let topic = sample.subject.strip_prefix("Re: ")?;
    samples
        .iter()
        .enumerate()
        .filter(|(_, other)| {
            (other.subject == topic || other.subject == sample.subject)
                && other.minutes_ago > sample.minutes_ago
        })
        .min_by_key(|(_, other)| other.minutes_ago)
        .map(|(i, _)| i)
}

/// The Message-ID of sample `index`.
fn demo_id(index: usize) -> String {
    format!("<demo-{}@avero.demo>", index + 1)
}

/// The ids a sample's References header lists: its ancestors, oldest
/// first.
fn ancestors(samples: &[Sample], index: usize) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = index;
    while let Some(parent) = parent_of(samples, current) {
        chain.insert(0, demo_id(parent));
        current = parent;
    }
    chain
}

/// Opens an in-memory store filled with the demo data.
pub fn seeded_store() -> Store {
    let store = Store::open_in_memory().expect("in-memory database");
    seed(&store);
    store
}

/// Fills `store` with the demo accounts, folders, messages and bodies.
pub fn seed(store: &Store) {
    let now = Local::now().timestamp();
    let mut ids = Vec::new();
    for (name, email, domain, auth) in ACCOUNTS {
        let server = |prefix: &str, port| ServerConfig {
            host: format!("{prefix}.{domain}"),
            port,
            username: email.to_string(),
            security: Security::Tls,
        };
        let account = store
            .add_account(NewAccount {
                name: name.to_string(),
                email: email.to_string(),
                imap: server("imap", 993),
                smtp: server("smtp", 465),
                auth,
            })
            .expect("demo account");
        if name == "Travail" {
            let _ = store.set_signature(account.id, WORK_SIGNATURE);
        }
        ids.push(account.id);
    }

    let samples = samples();
    for (index, sample) in samples.iter().enumerate() {
        let account = ids[sample.account];
        let folder_path = folder_for(sample.role);
        let folder = store
            .upsert_folder(account, folder_path, sample.role)
            .expect("demo folder");
        let uid = index as u32 + 1;
        let date = now - sample.minutes_ago * 60;
        let references = ancestors(&samples, index);
        let (unsubscribe, unsubscribe_post, list_id) = list_headers(sample.from.1);
        let raw = raw_message(sample, ACCOUNTS[sample.account].1, uid, date, &references);
        let message = NewMessage {
            uid,
            message_id: Some(demo_id(index)),
            subject: sample.subject.to_string(),
            from_name: Some(sample.from.0.to_string()),
            from_addr: sample.from.1.to_string(),
            date,
            seen: sample.seen,
            flagged: STARRED.contains(&sample.subject),
            has_attachments: sample.attachment.is_some(),
            headers: MessageHeaders {
                in_reply_to: references.last().cloned(),
                references: references.clone(),
                to: vec![ACCOUNTS[sample.account].1.to_string()],
                list_unsubscribe: unsubscribe.map(str::to_string),
                list_unsubscribe_post: unsubscribe_post.map(str::to_string),
                list_id: list_id.map(str::to_string),
                ..Default::default()
            },
            size: Some(raw.len() as u32),
        };
        store
            .insert_messages(folder.id, &[message])
            .expect("demo message");
        if let Some(row) = row_for(store, account, sample.role, date) {
            let _ = store.put_body(
                row,
                raw.as_bytes(),
                // Like the sync: the text, not the HTML it may come from.
                &avero_sync::parse::parse_body(raw.as_bytes()).text,
                sample.attachment.is_some(),
            );
        }
    }
    for &account in &ids {
        for role in [
            FolderRole::Sent,
            FolderRole::Drafts,
            FolderRole::Archive,
            FolderRole::Trash,
            FolderRole::Junk,
        ] {
            let _ = store.upsert_folder(account, folder_for(role), role);
        }
    }
    for rule in demo_rules(&ids) {
        let _ = store.add_rule(&rule);
    }
    seed_later(store, &ids);
    // "Perso" (with the cycling club) and "Pro".
    let _ = store.add_space("Perso", "house", 2, &[ids[0], ids[2]]);
    let _ = store.add_space("Pro", "briefcase", 0, &[ids[1]]);
}

/// "Chercher aussi sur le serveur" in the demo: an older message of the
/// account's archives about `query`, as a server would find one (added
/// once; searching again finds it again).
pub fn server_hits(store: &Store, account: AccountId, query: &str) -> ServerHits {
    let query = query.trim();
    let Ok(folder) = store.upsert_folder(
        account,
        folder_for(FolderRole::Archive),
        FolderRole::Archive,
    ) else {
        return ServerHits::default();
    };
    // One UID per search text, far above the seeded ones.
    let uid = 100_000
        + query
            .chars()
            .fold(0u32, |sum, c| sum.wrapping_mul(31).wrapping_add(c as u32))
            % 100_000;
    let mut added = 0;
    if !matches!(store.known_uids(folder.id, &[uid]), Ok(known) if known.contains(&uid)) {
        let date = Local::now().timestamp() - 400 * D * 60;
        let message_id = format!("<demo-search-{}-{uid}@avero.demo>", account.0);
        let subject = format!("{query} (message plus ancien)");
        let body = "Mode démo : ce message imite un mail trouvé par la recherche sur le \
                    serveur, qui n'était pas encore sur cet ordinateur.";
        let raw = format!(
            "From: Archives démo <archives@avero.demo>\r\nSubject: {subject}\r\n\
             Date: {}\r\nMessage-ID: {message_id}\r\nMIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n\
             {body}\r\n",
            Local
                .timestamp_opt(date, 0)
                .single()
                .map(|d| d.to_rfc2822())
                .unwrap_or_default()
        );
        let message = NewMessage {
            uid,
            message_id: Some(message_id),
            subject,
            from_name: Some("Archives démo".to_string()),
            from_addr: "archives@avero.demo".to_string(),
            date,
            seen: true,
            size: Some(raw.len() as u32),
            ..Default::default()
        };
        if store.insert_messages(folder.id, &[message]).is_ok() {
            added = 1;
            if let Ok(Some(row)) = store.message_row(folder.id, uid) {
                let _ = store.put_body(row, raw.as_bytes(), body, false);
            }
        }
    }
    ServerHits {
        added,
        rows: store.rows_of_uids(folder.id, &[uid]).unwrap_or_default(),
    }
}

/// Conversations snoozed by the demo ("Plus tard"), and until when.
const SNOOZED: [(&str, When); 2] = [
    ("Sortie de dimanche : on part à 8 h 30", When::Tonight),
    ("3 nouvelles adhésions au Vélo Club", When::Monday),
];

/// "Plus tard", a draft in the "Travail" drafts folder and a mail in
/// "Programmés".
fn seed_later(store: &Store, ids: &[AccountId]) {
    let now = Local::now();
    let inbox = store
        .list(
            &Listing::Folder(FolderRole::Inbox),
            AccountFilter::All,
            false,
            500,
            0,
        )
        .unwrap_or_default();
    for (subject, when) in SNOOZED {
        if let Some(item) = inbox.iter().find(|i| i.subject == subject) {
            let until = later::resolve(when, &now).timestamp();
            let _ = store.snooze(&item.row_ids, until);
        }
    }
    let Some(&work) = ids.get(1) else {
        return;
    };
    // A draft written "elsewhere": it is on the server only, and opens
    // in the composer from "Brouillons".
    if let Ok(drafts) =
        store.upsert_folder(work, folder_for(FolderRole::Drafts), FolderRole::Drafts)
    {
        let date = now.timestamp() - 2 * H * 60;
        let raw = format!(
            "From: Camille Martin <{work_addr}>\r\nTo: Julien Roche <julien.roche@atelier-lumen.fr>\r\n\
             Subject: Prévisionnel T4 (brouillon)\r\nDate: {}\r\nMessage-ID: <demo-draft@avero.demo>\r\n\
             MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\
             Content-Transfer-Encoding: 8bit\r\n\r\nJulien,\r\n\r\nVoici une première version des trois \
             scénarios. Il me manque encore les chiffres de novembre.\r\n",
            now.to_rfc2822(),
            work_addr = ACCOUNTS[1].1,
        );
        let message = NewMessage {
            uid: 1,
            message_id: Some("<demo-draft@avero.demo>".to_string()),
            subject: "Prévisionnel T4 (brouillon)".to_string(),
            from_name: Some("Camille Martin".to_string()),
            from_addr: ACCOUNTS[1].1.to_string(),
            date,
            seen: true,
            size: Some(raw.len() as u32),
            ..Default::default()
        };
        if store.insert_messages(drafts.id, &[message]).is_ok()
            && let Ok(Some(row)) = store.message_row(drafts.id, 1)
        {
            let text = "Julien,\n\nVoici une première version des trois scénarios. \
                        Il me manque encore les chiffres de novembre.";
            let _ = store.put_body(row, raw.as_bytes(), text, false);
        }
    }
    // A mail that leaves tomorrow morning.
    let fields = DraftFields {
        account: work,
        to: "Léa Bernard <lea.bernard@atelier-lumen.fr>".to_string(),
        subject: "Retours client Orsay".to_string(),
        body: format!(
            "Bonjour Léa,\n\nLe client valide la v3, avec deux petites retouches sur la page \
             d'accueil. On en parle à 9 h ?\n\n-- \n{WORK_SIGNATURE}"
        ),
        message_id: "<demo-scheduled@avero.demo>".to_string(),
        ..Default::default()
    };
    if let Ok(id) = store.save_draft(None, &fields, Some(&[]), now.timestamp()) {
        let _ = store.schedule_draft(id, later::resolve(When::Tomorrow, &now).timestamp());
    }
}

/// A few rules for the settings page.
fn demo_rules(ids: &[AccountId]) -> Vec<RuleDef> {
    let condition = |field, value: &str| Condition {
        field,
        value: value.to_string(),
    };
    vec![
        RuleDef {
            name: "Factures aux archives".to_string(),
            conditions: vec![
                condition(RuleField::Subject, "facture"),
                condition(RuleField::From, "facture@"),
            ],
            match_all: false,
            mark_read: true,
            move_to: Some(RuleMove::Archive),
            account: None,
        },
        RuleDef {
            name: "Notion sans notification".to_string(),
            conditions: vec![condition(RuleField::From, "makenotion.com")],
            match_all: true,
            mark_read: true,
            move_to: None,
            account: ids.get(1).copied(),
        },
        block_rule("newsletter@behance.net", "Behance"),
    ]
}

/// Server path of each demo folder.
fn folder_for(role: FolderRole) -> &'static str {
    match role {
        FolderRole::Inbox => "INBOX",
        FolderRole::Sent => "Envoyés",
        FolderRole::Drafts => "Brouillons",
        FolderRole::Archive => "Archives",
        FolderRole::Trash => "Corbeille",
        FolderRole::Junk => "Spam",
        FolderRole::Other => "Divers",
    }
}

/// The row of the message just stored (dates are unique per sample).
fn row_for(store: &Store, account: AccountId, role: FolderRole, date: i64) -> Option<i64> {
    store
        .unified(role, avero_core::AccountFilter::Only(account), 500, 0)
        .ok()?
        .into_iter()
        .find(|item| item.date == date)
        .and_then(|item| item.row_ids.first().copied())
}

/// A small RFC 5322 message, with one attachment when the sample has one
/// and the threading headers when it answers another (`references`).
fn raw_message(sample: &Sample, to: &str, uid: u32, date: i64, references: &[String]) -> String {
    let date = Local
        .timestamp_opt(date, 0)
        .single()
        .map(|d| d.to_rfc2822())
        .unwrap_or_default();
    let mut headers = format!(
        "From: {} <{}>\r\nTo: Camille Martin <{to}>\r\nSubject: {}\r\nDate: {date}\r\nMessage-ID: <demo-{uid}@avero.demo>\r\nMIME-Version: 1.0\r\n",
        sample.from.0, sample.from.1, sample.subject
    );
    if let Some(parent) = references.last() {
        headers.push_str(&format!(
            "In-Reply-To: {parent}\r\nReferences: {}\r\n",
            references.join(" ")
        ));
    }
    let body = sample.body.replace('\n', "\r\n");
    // A body written in HTML goes as such (a newsletter).
    let kind = if body.starts_with('<') {
        "html"
    } else {
        "plain"
    };
    match sample.attachment {
        None => format!(
            "{headers}Content-Type: text/{kind}; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{body}\r\n"
        ),
        Some((name, mime, size)) => {
            let filler = filler(size);
            format!(
                "{headers}Content-Type: multipart/mixed; boundary=\"avero-demo\"\r\n\r\n\
                 --avero-demo\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{body}\r\n\
                 --avero-demo\r\nContent-Type: {mime}; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Transfer-Encoding: 7bit\r\n\r\n{filler}\r\n\
                 --avero-demo--\r\n"
            )
        }
    }
}

/// `size` bytes of placeholder text, in 76-character lines.
fn filler(size: usize) -> String {
    let line = "Fichier de démonstration Avero : aucun contenu réel. ---------------------\r\n";
    let mut out = String::with_capacity(size + line.len());
    while out.len() < size {
        out.push_str(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use avero_core::AccountFilter;

    #[test]
    fn env_value_selects_the_mode() {
        assert_eq!(from_env_value(None), None);
        assert_eq!(from_env_value(Some("0")), None);
        assert_eq!(from_env_value(Some("1")), Some(DemoKind::Seeded));
        assert_eq!(from_env_value(Some("EMPTY")), Some(DemoKind::Empty));
        assert_eq!(
            from_env_value(Some("google-setup")),
            Some(DemoKind::Setup(avero_oauth::Provider::Google))
        );
    }

    #[test]
    fn seeded_store_has_three_accounts_and_cached_bodies() {
        let store = seeded_store();
        let accounts = store.accounts().unwrap();
        assert_eq!(accounts.len(), 3);
        let inbox = store
            .unified(FolderRole::Inbox, AccountFilter::All, 200, 0)
            .unwrap();
        assert!(inbox.len() >= 30, "{}", inbox.len());
        assert!(inbox.iter().all(|i| !i.snippet.is_empty()));
        assert!(inbox.iter().any(|i| i.has_attachments));
        let unread = store
            .unread_count(FolderRole::Inbox, AccountFilter::All)
            .unwrap();
        assert!((5..=15).contains(&unread), "{unread}");
        for account in &accounts {
            for role in [FolderRole::Sent, FolderRole::Trash, FolderRole::Archive] {
                assert!(store.folder_by_role(account.id, role).unwrap().is_some());
            }
        }
    }

    #[test]
    fn demo_has_conversations_across_folders() {
        let store = seeded_store();
        let threads = store
            .threads(FolderRole::Inbox, AccountFilter::All, 200, 0)
            .unwrap();
        let count = |subject: &str| {
            threads
                .iter()
                .find(|t| t.subject == subject)
                .map(|t| t.count)
        };
        assert_eq!(
            count("Re: Point budget T4 — à préparer pour jeudi"),
            Some(3)
        );
        assert_eq!(count("Re: Repas de dimanche"), Some(3));
        assert_eq!(count("Remboursement des maillots"), Some(2));
        assert_eq!(count("Re: Atelier design system"), Some(2));
        let budget = threads
            .iter()
            .find(|t| t.subject.starts_with("Re: Point budget"))
            .unwrap();
        assert_eq!(budget.senders, vec!["Julien Roche", "moi"]);
        assert!(!budget.seen);
        // The raw messages carry the same headers.
        let messages = store
            .thread_messages(
                budget.thread_id.unwrap(),
                FolderRole::Inbox,
                AccountFilter::All,
            )
            .unwrap();
        let (raw, _) = store.body(messages[2].row_ids[0]).unwrap().unwrap();
        let parsed = avero_sync::parse::parse_body(&raw);
        let references = parsed.references.unwrap_or_default();
        assert_eq!(references.split(' ').count(), 2, "{references}");
    }

    #[test]
    fn demo_fills_every_tab_and_favourites() {
        use avero_core::{Category, Listing};
        let store = seeded_store();
        for category in Category::ALL {
            let tab = store
                .list(&Listing::Tab(category), AccountFilter::All, true, 200, 0)
                .unwrap();
            assert!(tab.len() >= 3, "{category:?}: {}", tab.len());
        }
        let newsletters = store
            .list(
                &Listing::Tab(Category::Newsletter),
                AccountFilter::All,
                true,
                200,
                0,
            )
            .unwrap();
        assert!(
            newsletters
                .iter()
                .any(|i| i.from_addr == "newsletter@lemonde.fr")
        );
        let starred = store
            .list(&Listing::Starred, AccountFilter::All, true, 200, 0)
            .unwrap();
        assert_eq!(starred.len(), STARRED.len());
        // The archived one too.
        assert!(
            starred
                .iter()
                .any(|i| i.subject == "Bienvenue à l'Atelier Lumen")
        );
    }

    #[test]
    fn demo_has_reminders_a_draft_and_a_scheduled_mail() {
        let store = seeded_store();
        let snoozed = store
            .list(&Listing::Snoozed, AccountFilter::All, true, 200, 0)
            .unwrap();
        assert_eq!(snoozed.len(), SNOOZED.len());
        assert!(snoozed.iter().all(|item| item.snoozed_until.is_some()));
        let drafts = store
            .unified(FolderRole::Drafts, AccountFilter::All, 200, 0)
            .unwrap();
        assert_eq!(drafts.len(), 1);
        let (raw, _) = store.body(drafts[0].row_ids[0]).unwrap().unwrap();
        let parsed = avero_sync::parse::parse_body(&raw);
        assert_eq!(parsed.to[0].addr, "julien.roche@atelier-lumen.fr");
        assert!(parsed.text.contains("trois scénarios"), "{}", parsed.text);
        let scheduled = store.scheduled().unwrap();
        assert_eq!(scheduled.len(), 1);
        assert!(scheduled[0].send_at.unwrap() > Local::now().timestamp());
    }

    #[test]
    fn demo_has_two_spaces() {
        let store = seeded_store();
        let spaces = store.spaces().unwrap();
        let names: Vec<&str> = spaces.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Perso", "Pro"]);
        assert_eq!(spaces[0].accounts.len(), 2);
        assert_eq!(spaces[1].accounts.len(), 1);
    }

    #[test]
    fn demo_server_search_adds_one_older_message_once() {
        let store = seeded_store();
        let account = store.accounts().unwrap()[1].id;
        let first = server_hits(&store, account, "budget");
        assert_eq!(first.added, 1);
        assert_eq!(first.rows.len(), 1);
        let again = server_hits(&store, account, "budget");
        assert_eq!(again.added, 0);
        assert_eq!(again.rows, first.rows);
        let found = store
            .search_with("budget", None, AccountFilter::All, &first.rows, 50)
            .unwrap();
        assert!(
            found
                .iter()
                .any(|i| i.subject == "budget (message plus ancien)")
        );
        let (raw, _) = store.body(first.rows[0]).unwrap().unwrap();
        let parsed = avero_sync::parse::parse_body(&raw);
        assert!(parsed.text.contains("Mode démo"), "{}", parsed.text);
    }

    #[test]
    fn demo_bodies_parse_with_attachments() {
        let store = seeded_store();
        let item = store
            .unified(FolderRole::Inbox, AccountFilter::All, 200, 0)
            .unwrap()
            .into_iter()
            .find(|i| i.has_attachments)
            .unwrap();
        let (raw, _) = store.body(item.row_ids[0]).unwrap().unwrap();
        let parsed = avero_sync::parse::parse_body(&raw);
        assert_eq!(parsed.attachments.len(), 1);
        assert!(parsed.text.len() > 20);
        assert_eq!(parsed.subject, item.subject);
    }
}

//! Base data types: accounts, folders, messages.

/// Database identifier of an account.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountId(pub i64);

/// Database identifier of a space (a named group of accounts).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpaceId(pub i64);

/// Database identifier of a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FolderId(pub i64);

/// RGB colour used to tell accounts apart in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Fixed palette; the first colour is #3B82F6.
const PALETTE: [AccountColor; 8] = [
    AccountColor {
        r: 0x3B,
        g: 0x82,
        b: 0xF6,
    }, // blue
    AccountColor {
        r: 0xEF,
        g: 0x44,
        b: 0x44,
    }, // red
    AccountColor {
        r: 0x10,
        g: 0xB9,
        b: 0x81,
    }, // green
    AccountColor {
        r: 0xF5,
        g: 0x9E,
        b: 0x0B,
    }, // amber
    AccountColor {
        r: 0x8B,
        g: 0x5C,
        b: 0xF6,
    }, // violet
    AccountColor {
        r: 0xEC,
        g: 0x48,
        b: 0x99,
    }, // pink
    AccountColor {
        r: 0x06,
        g: 0xB6,
        b: 0xD4,
    }, // cyan
    AccountColor {
        r: 0x84,
        g: 0xCC,
        b: 0x16,
    }, // lime
];

impl AccountColor {
    /// How many colours the palette holds.
    pub const COUNT: usize = PALETTE.len();

    /// Returns the colour at `index`, cycling through the 8 fixed colours.
    pub fn palette(index: usize) -> AccountColor {
        PALETTE[index % PALETTE.len()]
    }
}

/// How the connection to a server is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// TLS from the first byte (implicit TLS, ports 993 and 465).
    Tls,
    /// Plain connection upgraded with STARTTLS, which is required (port 587).
    StartTls,
}

impl Security {
    /// Stable text form, used for storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Security::Tls => "tls",
            Security::StartTls => "starttls",
        }
    }

    /// Parses the text form; unknown text gives `Tls` (the safest choice).
    pub fn parse(s: &str) -> Security {
        match s {
            "starttls" => Security::StartTls,
            _ => Security::Tls,
        }
    }
}

/// Connection settings of an IMAP or SMTP server. Holds no secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub security: Security,
}

/// How an account proves who it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// A password (or an app password) kept in the `SecretStore`.
    Password,
    /// OAuth2 with Google; the refresh token is kept in the `SecretStore`.
    OAuthGoogle,
    /// OAuth2 with Microsoft; the refresh token is kept in the `SecretStore`.
    OAuthMicrosoft,
}

impl AuthKind {
    /// Stable text form, used for storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthKind::Password => "password",
            AuthKind::OAuthGoogle => "oauth-google",
            AuthKind::OAuthMicrosoft => "oauth-microsoft",
        }
    }

    /// Parses the text form; unknown text gives `Password`.
    pub fn parse(s: &str) -> AuthKind {
        match s {
            "oauth-google" => AuthKind::OAuthGoogle,
            "oauth-microsoft" => AuthKind::OAuthMicrosoft,
            _ => AuthKind::Password,
        }
    }
}

/// What is sent to a server to log in. Lives in memory only.
#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    Password(String),
    OAuth2 { user: String, access_token: String },
}

/// Written by hand so that secrets never show up in logs.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credentials::Password(_) => f.write_str("Password(***)"),
            Credentials::OAuth2 { user, .. } => f
                .debug_struct("OAuth2")
                .field("user", user)
                .field("access_token", &"***")
                .finish(),
        }
    }
}

/// An account that has not been saved yet (no id, no colour).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAccount {
    pub name: String,
    pub email: String,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    pub auth: AuthKind,
}

/// A saved account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: AccountId,
    pub name: String,
    pub email: String,
    pub color: AccountColor,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    pub auth: AuthKind,
    /// Added under the messages written from this account, after a
    /// `-- ` line (empty: none).
    pub signature: String,
}

/// What a folder is used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderRole {
    Inbox,
    Sent,
    Drafts,
    Archive,
    Trash,
    Junk,
    Other,
}

impl FolderRole {
    /// Stable text form, used for storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            FolderRole::Inbox => "inbox",
            FolderRole::Sent => "sent",
            FolderRole::Drafts => "drafts",
            FolderRole::Archive => "archive",
            FolderRole::Trash => "trash",
            FolderRole::Junk => "junk",
            FolderRole::Other => "other",
        }
    }

    /// Parses the text form; unknown text gives `Other`.
    pub fn parse(s: &str) -> FolderRole {
        match s {
            "inbox" => FolderRole::Inbox,
            "sent" => FolderRole::Sent,
            "drafts" => FolderRole::Drafts,
            "archive" => FolderRole::Archive,
            "trash" => FolderRole::Trash,
            "junk" => FolderRole::Junk,
            _ => FolderRole::Other,
        }
    }
}

/// A mailbox folder of an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub id: FolderId,
    pub account_id: AccountId,
    pub path: String,
    pub role: FolderRole,
    pub uid_validity: Option<u32>,
    pub last_seen_uid: Option<u32>,
}

/// A message header fetched from the server, not yet stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewMessage {
    pub uid: u32,
    pub message_id: Option<String>,
    pub subject: String,
    pub from_name: Option<String>,
    pub from_addr: String,
    /// Unix time in seconds.
    pub date: i64,
    pub seen: bool,
    /// Starred (`\Flagged` on the server).
    pub flagged: bool,
    /// Guessed from the header block (see `avero_sync::parse`).
    pub has_attachments: bool,
    /// Threading, mailing-list and recipient headers.
    pub headers: MessageHeaders,
    /// Size of the whole message on the server (RFC822.SIZE), in bytes,
    /// when the server said it.
    pub size: Option<u32>,
}

/// The headers kept besides those shown in the list: they link a message
/// to its conversation, tell newsletters and notifications apart, and
/// say how to unsubscribe. Raw values have their line folding removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageHeaders {
    /// The parent's Message-ID, with angle brackets.
    pub in_reply_to: Option<String>,
    /// The ids of the References header, with angle brackets, oldest
    /// (the conversation's first message) first.
    pub references: Vec<String>,
    /// Addresses of the To header.
    pub to: Vec<String>,
    /// The first (top-most) Delivered-To header: the mailbox that
    /// really received the message.
    pub delivered_to: Option<String>,
    /// For example `<mailto:u@list.example>, <https://list.example/u>`.
    pub list_unsubscribe: Option<String>,
    /// `List-Unsubscribe=One-Click` when one-click unsubscribe (RFC 8058)
    /// is offered.
    pub list_unsubscribe_post: Option<String>,
    pub list_id: Option<String>,
    /// For example `bulk` or `list`.
    pub precedence: Option<String>,
    /// For example `auto-generated` (RFC 3834).
    pub auto_submitted: Option<String>,
}

/// The flags of one message, as the server reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerFlags {
    pub uid: u32,
    /// `\Seen`: read.
    pub seen: bool,
    /// `\Flagged`: starred.
    pub flagged: bool,
}

/// What a list shows, besides the accounts (see `Store::list`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// Every message of a folder role.
    Folder(FolderRole),
    /// The inbox messages of one tab (see `crate::category`).
    Tab(crate::Category),
    /// The starred messages of every folder but the trash and spam ones.
    Starred,
    /// Every message from this address (any case), in every folder.
    Sender(String),
    /// The snoozed conversations ("Plus tard"), the next to wake first.
    Snoozed,
}

/// Which accounts the message list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountFilter {
    All,
    Only(AccountId),
    /// The accounts of a space (none if the space has none).
    Space(SpaceId),
}

/// A space: a name, an icon, a colour and the accounts it shows ("Perso",
/// "Pro"). An account can be in several spaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Space {
    pub id: SpaceId,
    pub name: String,
    /// The name of its icon (the app knows which ones exist).
    pub icon: String,
    /// Its colour, an index in the accounts' palette (see
    /// `AccountColor::palette`).
    pub color: usize,
    /// Its accounts, by account id.
    pub accounts: Vec<AccountId>,
}

impl Space {
    /// Whether the space shows this account.
    pub fn contains(&self, account: AccountId) -> bool {
        self.accounts.contains(&account)
    }
}

/// One line of the unified inbox (duplicates across accounts merged).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedItem {
    pub row_ids: Vec<i64>,
    pub accounts: Vec<AccountId>,
    pub subject: String,
    pub from_display: String,
    pub from_addr: String,
    pub date: i64,
    pub seen: bool,
    /// Starred: one of its rows has `\Flagged`.
    pub flagged: bool,
    pub has_attachments: bool,
    /// Start of the body text when the body is cached, else empty.
    pub snippet: String,
    /// The conversation this item stands for, when the list groups
    /// conversations (see `Store::threads`); `None` for a single message.
    pub thread_id: Option<i64>,
    /// Number of messages in the conversation (1 for a single message).
    pub count: usize,
    /// Who wrote in the conversation, oldest first, each person once;
    /// the user's own addresses are `moi`. Just the sender for a single
    /// message.
    pub senders: Vec<String>,
    /// When it comes back, if it is snoozed ("Plus tard"; Unix time).
    pub snoozed_until: Option<i64>,
}

/// A conversation whose snooze just ended (see `Store::wake_snoozed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Woken {
    /// Every copy of its newest message: marked unread again.
    pub rows: Vec<i64>,
    pub from_name: Option<String>,
    pub from_addr: String,
    pub subject: String,
}

/// Where the copy of a draft lives in a server's drafts folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftCopy {
    pub account: AccountId,
    pub folder: String,
    pub uid: u32,
}

/// What the user wrote in a draft, as the composer holds it (addresses
/// as typed, not checked yet).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftFields {
    pub account: AccountId,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    /// The Message-ID of its copies on the server (one per draft, kept
    /// from one save to the next).
    pub message_id: String,
}

/// A draft kept on this computer, possibly scheduled (see `schema_v9.sql`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRecord {
    pub id: i64,
    pub fields: DraftFields,
    /// A composer shows it.
    pub open: bool,
    /// Changed since its copy on the server (or it has none).
    pub server_dirty: bool,
    pub server: Option<DraftCopy>,
    /// When to send it ("Programmés"), Unix time.
    pub send_at: Option<i64>,
    /// When it really left (kept only when that was late).
    pub sent_at: Option<i64>,
    /// Why sending failed.
    pub error: Option<String>,
    pub updated_at: i64,
}

/// A file attached to a draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftFile {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

/// One sender of "Grand nettoyage": how much mail they left in the
/// user's folders (trash, spam, sent and drafts left out).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderStats {
    /// The address, trimmed and in lower case.
    pub addr: String,
    /// The name of their most recent message (else the address).
    pub name: String,
    /// Number of messages (each copy counts).
    pub count: usize,
    /// How many were never opened (still unread).
    pub unread: usize,
    /// Total size in bytes (messages of unknown size count 0).
    pub size: u64,
    /// One of their messages offers to unsubscribe (List-Unsubscribe).
    pub can_unsubscribe: bool,
}

/// Where one stored message lives on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRef {
    pub row_id: i64,
    pub account_id: AccountId,
    pub folder_id: FolderId,
    pub folder_path: String,
    pub uid: u32,
    /// The folder's UIDVALIDITY when `uid` was read (`None`: never
    /// synced). If the server's is another one, `uid` names another
    /// message now: nothing must be done with it.
    pub uid_validity: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_role_roundtrip() {
        for r in [
            FolderRole::Inbox,
            FolderRole::Sent,
            FolderRole::Drafts,
            FolderRole::Archive,
            FolderRole::Trash,
            FolderRole::Junk,
            FolderRole::Other,
        ] {
            assert_eq!(FolderRole::parse(r.as_str()), r);
        }
        assert_eq!(FolderRole::parse("nimporte"), FolderRole::Other);
    }

    #[test]
    fn security_and_auth_roundtrip() {
        for s in [Security::Tls, Security::StartTls] {
            assert_eq!(Security::parse(s.as_str()), s);
        }
        assert_eq!(Security::parse("plain"), Security::Tls);
        for a in [
            AuthKind::Password,
            AuthKind::OAuthGoogle,
            AuthKind::OAuthMicrosoft,
        ] {
            assert_eq!(AuthKind::parse(a.as_str()), a);
        }
        assert_eq!(AuthKind::parse("?"), AuthKind::Password);
    }

    #[test]
    fn credentials_debug_hides_secrets() {
        let password = format!("{:?}", Credentials::Password("hunter2".into()));
        assert!(!password.contains("hunter2"), "{password}");
        assert!(password.contains("***"));
        let oauth = format!(
            "{:?}",
            Credentials::OAuth2 {
                user: "a@x".into(),
                access_token: "ya29.token".into(),
            }
        );
        assert!(!oauth.contains("ya29"), "{oauth}");
        assert!(oauth.contains("a@x") && oauth.contains("***"));
    }

    #[test]
    fn palette_cycles() {
        assert_eq!(
            AccountColor::palette(0),
            AccountColor {
                r: 0x3B,
                g: 0x82,
                b: 0xF6
            }
        );
        assert_eq!(AccountColor::palette(8), AccountColor::palette(0));
        assert_ne!(AccountColor::palette(1), AccountColor::palette(0));
    }
}

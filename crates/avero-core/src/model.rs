//! Base data types: accounts, folders, messages.

/// Database identifier of an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountId(pub i64);

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
    /// Returns the colour at `index`, cycling through the 8 fixed colours.
    pub fn palette(index: usize) -> AccountColor {
        PALETTE[index % PALETTE.len()]
    }
}

/// Connection settings of an IMAP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
}

/// An account that has not been saved yet (no id, no colour).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAccount {
    pub name: String,
    pub email: String,
    pub imap: ServerConfig,
}

/// A saved account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: AccountId,
    pub name: String,
    pub email: String,
    pub color: AccountColor,
    pub imap: ServerConfig,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMessage {
    pub uid: u32,
    pub message_id: Option<String>,
    pub subject: String,
    pub from_name: Option<String>,
    pub from_addr: String,
    /// Unix time in seconds.
    pub date: i64,
    pub seen: bool,
}

/// Which accounts the message list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountFilter {
    All,
    Only(AccountId),
}

/// One line of the unified inbox (duplicates across accounts merged).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedItem {
    pub row_ids: Vec<i64>,
    pub accounts: Vec<AccountId>,
    pub subject: String,
    pub from_display: String,
    pub date: i64,
    pub seen: bool,
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

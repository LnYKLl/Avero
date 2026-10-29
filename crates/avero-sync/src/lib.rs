//! Talking to mail servers: IMAP synchronisation and actions, SMTP
//! sending, MIME parsing, watching the inbox and searching the server.

pub mod imap;
pub mod parse;
pub mod search;
pub mod smtp;
pub mod source;
pub mod sync;
#[cfg(feature = "test-ca")]
mod test_ca;
pub mod watch;

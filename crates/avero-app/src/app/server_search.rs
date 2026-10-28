//! "Chercher aussi sur le serveur": under the results of a local search,
//! one job per account of the list asks its server, which stores the
//! messages it finds that were missing here. They join the results.

use super::{App, Message};
use crate::worker::{Event, Job};
use avero_core::{AccountFilter, AccountId};
use iced::Task;
use std::time::Duration;
use tokio::sync::oneshot;

/// How long the demo pretends to search.
const DEMO_DELAY: Duration = Duration::from_millis(900);

/// Where the search on the server stands, for the search shown.
#[derive(Debug, Default)]
pub enum ServerSearch {
    /// Not asked (the button is shown).
    #[default]
    Idle,
    Running {
        /// The search and the accounts it was asked for.
        query: String,
        scope: AccountFilter,
        /// Accounts that have not answered yet.
        waiting: usize,
        /// Messages added so far.
        added: usize,
        /// Accounts that could not search.
        failed: usize,
        /// Never read: dropping them (with this state) stops the searches
        /// still running.
        _cancels: Vec<oneshot::Sender<()>>,
    },
    Done {
        query: String,
        scope: AccountFilter,
        added: usize,
        failed: usize,
    },
}

impl ServerSearch {
    /// Whether it is about this search on these accounts.
    fn is_for(&self, query: Option<&str>, scope: AccountFilter) -> bool {
        match self {
            ServerSearch::Idle => true,
            ServerSearch::Running {
                query: q, scope: s, ..
            }
            | ServerSearch::Done {
                query: q, scope: s, ..
            } => Some(q.as_str()) == query && *s == scope,
        }
    }

    /// Whether some server is still searching.
    pub fn is_running(&self) -> bool {
        matches!(self, ServerSearch::Running { .. })
    }
}

/// What is said under the results once the servers answered.
pub fn server_search_result(added: usize, failed: usize) -> String {
    let found = match added {
        0 => "Aucun autre mail trouvé sur le serveur".to_string(),
        1 => "1 mail trouvé sur le serveur".to_string(),
        n => format!("{n} mails trouvés sur le serveur"),
    };
    match failed {
        0 => found,
        1 => format!("{found} (un compte n'a pas pu chercher)"),
        n => format!("{found} ({n} comptes n'ont pas pu chercher)"),
    }
}

impl App {
    /// "Chercher aussi sur le serveur": one job per account of the list.
    pub(super) fn search_on_server(&mut self) -> Task<Message> {
        let Some(query) = self.search_query.clone() else {
            return Task::none();
        };
        if self.server_search.is_running() || avero_sync::imap::search_criteria(&query).is_none() {
            return Task::none();
        }
        let scope = self.scope();
        let accounts: Vec<AccountId> = self
            .space_accounts()
            .iter()
            .map(|a| a.id)
            .filter(|id| match scope {
                AccountFilter::Only(only) => *id == only,
                _ => true,
            })
            .collect();
        if accounts.is_empty() {
            return Task::none();
        }
        self.server_generation += 1;
        let generation = self.server_generation;
        self.server_rows.clear();
        let mut cancels = Vec::new();
        let mut tasks = Vec::new();
        for account in &accounts {
            let account = *account;
            let (cancel, cancelled) = oneshot::channel();
            cancels.push(cancel);
            let task = if self.is_demo() {
                let hits = crate::demo::server_hits(&self.store, account, &query);
                Task::perform(super::sleep(DEMO_DELAY), move |()| {
                    Message::ServerFound(generation, account, Event::Found(Ok(hits.clone())))
                })
            } else {
                let query = query.clone();
                self.request(account, |reply| Job::SearchServer {
                    query,
                    cancel: cancelled,
                    reply,
                })
                .map(move |message| match message {
                    Message::Worker(account, event) => {
                        Message::ServerFound(generation, account, event)
                    }
                    other => other,
                })
            };
            tasks.push(task);
        }
        self.server_search = ServerSearch::Running {
            query,
            scope,
            waiting: accounts.len(),
            added: 0,
            failed: 0,
            _cancels: cancels,
        };
        Task::batch(tasks)
    }

    /// An account's server answered (or its worker stopped).
    pub(super) fn server_found(&mut self, generation: u64, event: Event) -> Task<Message> {
        if generation != self.server_generation {
            return Task::none();
        }
        let ServerSearch::Running {
            query,
            scope,
            waiting,
            added,
            failed,
            ..
        } = &mut self.server_search
        else {
            return Task::none();
        };
        match event {
            Event::Found(Ok(hits)) => {
                *added += hits.added;
                self.server_rows.extend(hits.rows);
            }
            Event::Found(Err(_)) | Event::Gone => *failed += 1,
            // Nothing else answers a search.
            _ => return Task::none(),
        }
        *waiting = waiting.saturating_sub(1);
        if *waiting == 0 {
            self.server_search = ServerSearch::Done {
                query: std::mem::take(query),
                scope: *scope,
                added: *added,
                failed: *failed,
            };
        }
        self.reload_now();
        Task::none()
    }

    /// Forgets the search on the server (stopping what still runs) when
    /// the list no longer shows the search it was made for.
    pub(super) fn forget_stale_server_search(&mut self) {
        let scope = self.scope();
        if !self
            .server_search
            .is_for(self.search_query.as_deref(), scope)
        {
            // Dropping the senders stops the jobs still running.
            self.server_search = ServerSearch::Idle;
            self.server_rows.clear();
            self.server_generation += 1;
        }
    }
}

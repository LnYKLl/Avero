//! "Spam" and "Pas spam", the local spam filter, and the user's rules:
//! what happens to new mail after each sync, the rules page of the
//! settings and "Appliquer maintenant".

use super::{App, Backend, Message, Overlay};
use crate::worker::Destination;
use avero_core::rules::{self, Condition, RuleDef, RuleFacts, RuleField, RuleMove};
use avero_core::spam;
use avero_core::store::Store;
use avero_core::{AccountFilter, AccountId, FolderRole};
use avero_sync::sync::Arrival;
use iced::Task;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use tokio::sync::oneshot;

/// How many of the newest messages of the spam folder and of the inbox
/// the filter learns from, once per account.
pub const BOOTSTRAP_LIMIT: usize = 500;

/// Messages of the rules page and of the rule editor.
#[derive(Debug, Clone)]
pub enum RuleMsg {
    /// Opens an empty rule.
    New,
    /// Opens a saved rule.
    Edit(i64),
    Name(String),
    /// What condition `.0` looks at.
    Field(usize, RuleField),
    /// The text condition `.0` looks for.
    Value(usize, String),
    AddCondition,
    RemoveCondition(usize),
    MatchAll(bool),
    MarkRead(bool),
    MoveTo(Option<RuleMove>),
    /// The account it applies to (`None`: all of them).
    Scope(Option<AccountId>),
    Save,
    Toggle(i64, bool),
    Delete(i64),
    /// Earlier (`true`) or later in the order.
    Shift(i64, bool),
    /// "Appliquer maintenant": counts, then asks.
    Apply(i64),
    ConfirmApply,
}

/// The rule being written or changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleForm {
    /// The saved rule changed, or `None` for a new one.
    pub editing: Option<i64>,
    pub name: String,
    pub conditions: Vec<Condition>,
    pub match_all: bool,
    pub mark_read: bool,
    pub move_to: Option<RuleMove>,
    pub account: Option<AccountId>,
    pub error: Option<String>,
}

impl RuleForm {
    /// An empty rule: one sender condition, archive.
    pub fn new() -> RuleForm {
        RuleForm {
            editing: None,
            name: String::new(),
            conditions: vec![Condition {
                field: RuleField::From,
                value: String::new(),
            }],
            match_all: true,
            mark_read: false,
            move_to: Some(RuleMove::Archive),
            account: None,
            error: None,
        }
    }

    /// The form of a saved rule.
    pub fn of(id: i64, def: &RuleDef) -> RuleForm {
        RuleForm {
            editing: Some(id),
            name: def.name.clone(),
            conditions: def.conditions.clone(),
            match_all: def.match_all,
            mark_read: def.mark_read,
            move_to: def.move_to,
            account: def.account,
            error: None,
        }
    }

    /// The rule to save, or what is missing. Empty conditions are
    /// dropped; a rule without a name is named after its first one.
    pub fn to_def(&self) -> Result<RuleDef, String> {
        let conditions: Vec<Condition> = self
            .conditions
            .iter()
            .filter(|c| !c.value.trim().is_empty())
            .map(|c| Condition {
                field: c.field,
                value: c.value.trim().to_string(),
            })
            .collect();
        let Some(first) = conditions.first() else {
            return Err("Indiquez au moins une condition".to_string());
        };
        if !self.mark_read && self.move_to.is_none() {
            return Err("Choisissez au moins une action".to_string());
        }
        let name = match self.name.trim() {
            "" => format!("{} contient « {} »", field_name(first.field), first.value),
            name => name.to_string(),
        };
        Ok(RuleDef {
            name,
            conditions,
            match_all: self.match_all,
            mark_read: self.mark_read,
            move_to: self.move_to,
            account: self.account,
        })
    }
}

impl Default for RuleForm {
    fn default() -> RuleForm {
        RuleForm::new()
    }
}

/// What a condition looks at, as the editor names it.
pub fn field_name(field: RuleField) -> &'static str {
    match field {
        RuleField::From => "Expéditeur",
        RuleField::To => "Destinataire",
        RuleField::Subject => "Objet",
        RuleField::FromAddress => "Adresse exacte",
    }
}

/// How a condition compares, as the editor says it: the exact address
/// "est", the others "contient".
pub fn field_verb(field: RuleField) -> &'static str {
    match field {
        RuleField::FromAddress => "est",
        RuleField::From | RuleField::To | RuleField::Subject => "contient",
    }
}

/// Where a rule moves mail, as the editor names it.
pub fn move_name(to: Option<RuleMove>) -> &'static str {
    match to {
        None => "Ne pas déplacer",
        Some(RuleMove::Archive) => "Archiver",
        Some(RuleMove::Trash) => "Mettre à la corbeille",
        Some(RuleMove::Junk) => "Placer dans le spam",
    }
}

/// The worker's destination of a rule's move.
fn destination(to: RuleMove) -> Destination {
    match to {
        RuleMove::Archive => Destination::Archive,
        RuleMove::Trash => Destination::Trash,
        RuleMove::Junk => Destination::Junk,
    }
}

/// What to do with some messages: mark them read, move them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Plan {
    pub read: Vec<i64>,
    pub moves: BTreeMap<Destination, Vec<i64>>,
    /// How many the spam filter moved.
    pub spam: usize,
}

impl Plan {
    /// Adds what the rules decided for one message.
    fn add(&mut self, facts: &RuleFacts, outcome: rules::RuleOutcome) {
        if outcome.mark_read && !facts.seen {
            self.read.push(facts.row);
        }
        if let Some(to) = outcome.move_to {
            self.moves
                .entry(destination(to))
                .or_default()
                .push(facts.row);
        }
    }

    /// The rows it touches.
    fn rows(&self) -> HashSet<i64> {
        self.read
            .iter()
            .chain(self.moves.values().flatten())
            .copied()
            .collect()
    }
}

impl App {
    pub(super) fn update_rules(&mut self, message: RuleMsg) -> Task<Message> {
        match message {
            RuleMsg::New => {
                self.overlay = Some(Overlay::RuleEditor(RuleForm::new()));
                Task::none()
            }
            RuleMsg::Edit(id) => {
                if let Some(rule) = self.rules.iter().find(|r| r.id == id) {
                    self.overlay = Some(Overlay::RuleEditor(RuleForm::of(id, &rule.def)));
                }
                Task::none()
            }
            RuleMsg::Save => self.save_rule(),
            RuleMsg::Toggle(id, on) => {
                let result = self.store.set_rule_enabled(id, on);
                self.after_rule_change(result)
            }
            RuleMsg::Delete(id) => {
                let result = self.store.delete_rule(id);
                let task = self.after_rule_change(result);
                Task::batch([task, self.toast("Règle supprimée", false)])
            }
            RuleMsg::Shift(id, up) => {
                let result = self.store.move_rule(id, up);
                self.after_rule_change(result)
            }
            RuleMsg::Apply(id) => self.ask_apply_rule(id),
            RuleMsg::ConfirmApply => {
                let Some(Overlay::ConfirmApplyRule { rule, .. }) = self.overlay.take() else {
                    return Task::none();
                };
                self.apply_rule_now(rule)
            }
            edit => {
                if let Some(Overlay::RuleEditor(form)) = &mut self.overlay {
                    edit_form(form, edit);
                }
                Task::none()
            }
        }
    }

    /// Reads the rules again (after a change), or says what failed.
    pub(super) fn refresh_rules(&mut self) {
        self.rules = self.store.rules().unwrap_or_else(|e| {
            eprintln!("Impossible de lire les règles : {e}");
            Vec::new()
        });
    }

    fn after_rule_change(
        &mut self,
        result: Result<(), avero_core::store::StoreError>,
    ) -> Task<Message> {
        self.refresh_rules();
        match result {
            Ok(()) => Task::none(),
            Err(e) => self.toast(format!("Erreur de la base locale : {e}"), true),
        }
    }

    fn save_rule(&mut self) -> Task<Message> {
        let Some(Overlay::RuleEditor(form)) = &mut self.overlay else {
            return Task::none();
        };
        let def = match form.to_def() {
            Ok(def) => def,
            Err(error) => {
                form.error = Some(error);
                return Task::none();
            }
        };
        let result = match form.editing {
            Some(id) => self.store.update_rule(id, &def),
            None => self.store.add_rule(&def).map(|_| ()),
        };
        if let Err(e) = result {
            form.error = Some(format!("Erreur de la base locale : {e}"));
            return Task::none();
        }
        self.overlay = None;
        self.refresh_rules();
        self.toast("Règle enregistrée", false)
    }

    /// What `rule` would change in the inbox now: the rows, with what to
    /// do to them.
    fn rule_plan(&self, rule: i64) -> Plan {
        let Some(rule) = self.rules.iter().find(|r| r.id == rule) else {
            return Plan::default();
        };
        let mut plan = Plan::default();
        let mut forced = rule.clone();
        // "Appliquer maintenant" works even on a rule switched off.
        forced.enabled = true;
        for facts in self
            .store
            .inbox_facts(AccountFilter::All)
            .unwrap_or_default()
        {
            let outcome = rules::outcome(std::slice::from_ref(&forced), &facts);
            if outcome.changes(&facts) {
                plan.add(&facts, outcome);
            }
        }
        plan
    }

    /// "Appliquer maintenant": says how many messages of the inbox the
    /// rule changes, before doing it.
    fn ask_apply_rule(&mut self, id: i64) -> Task<Message> {
        let count = self.rule_plan(id).rows().len();
        if count == 0 {
            return self.toast("Aucun mail de la réception n'est concerné", false);
        }
        let name = self
            .rules
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.def.name.clone())
            .unwrap_or_default();
        self.overlay = Some(Overlay::ConfirmApplyRule {
            rule: id,
            name,
            count,
        });
        Task::none()
    }

    fn apply_rule_now(&mut self, id: i64) -> Task<Message> {
        let plan = self.rule_plan(id);
        let count = plan.rows().len();
        let task = self.run_plan(plan);
        let toast = self.toast(
            if count > 1 {
                format!("Règle appliquée à {count} mails")
            } else {
                "Règle appliquée à 1 mail".to_string()
            },
            false,
        );
        Task::batch([task, toast])
    }

    /// Marks read and moves what the plan says (locally first, then on
    /// the servers, one job per account).
    pub(super) fn run_plan(&mut self, plan: Plan) -> Task<Message> {
        let mut tasks = Vec::new();
        if !plan.read.is_empty() {
            let _ = self.store.set_seen(&plan.read, true);
            tasks.push(self.remote_seen(&plan.read, true));
        }
        for (to, rows) in plan.moves {
            tasks.extend(self.move_rows(&rows, to));
        }
        self.reload_now();
        Task::batch(tasks)
    }

    // ------------------------------------------------------------ new mail

    /// New mail of `account` after a sync: the rules first, then the
    /// spam filter, then a notification for what is left.
    pub(super) fn new_mail(&mut self, account: AccountId, arrivals: Vec<Arrival>) -> Task<Message> {
        let uids: Vec<u32> = arrivals.iter().map(|a| a.uid).collect();
        let facts = self.store.arrival_facts(account, &uids).unwrap_or_default();
        let plan = self.plan_for_new_mail(&facts);
        let handled = plan.rows();
        let kept: Vec<Arrival> = arrivals
            .into_iter()
            .filter(|arrival| {
                !facts
                    .iter()
                    .any(|f| handled.contains(&f.row) && f.uid == arrival.uid)
            })
            .collect();
        self.notify(account, &kept);
        let spam = plan.spam;
        let task = self.run_plan(plan);
        if spam == 0 {
            return task;
        }
        let toast = self.toast(
            if spam > 1 {
                format!("{spam} nouveaux mails placés dans le spam")
            } else {
                "1 nouveau mail placé dans le spam".to_string()
            },
            false,
        );
        Task::batch([task, toast])
    }

    /// What the enabled rules, then the spam filter (if on), do to new
    /// messages. The filter only looks at what no rule moved, and never
    /// at a sender the user wrote to or said "Pas spam" for.
    pub(super) fn plan_for_new_mail(&self, facts: &[RuleFacts]) -> Plan {
        let mut plan = Plan::default();
        for f in facts {
            let outcome = rules::outcome(&self.rules, f);
            plan.add(f, outcome);
            if outcome.move_to.is_some() || !self.settings.spam_filter {
                continue;
            }
            let exempt = self.store.spam_exempt(&f.from_addr).unwrap_or(true);
            if !exempt && spam::is_spam(self.store.spam_probability(f.row).ok().flatten()) {
                plan.moves.entry(Destination::Junk).or_default().push(f.row);
                plan.spam += 1;
            }
        }
        plan
    }

    // --------------------------------------------------------- spam buttons

    /// Whether every copy of the selection is in a spam folder ("Pas
    /// spam" is offered then).
    pub fn selection_in_junk(&self) -> bool {
        let refs = self.refs(&self.action_rows());
        !refs.is_empty() && refs.iter().all(|r| self.is_in(r, Destination::Junk))
    }

    /// "Spam" (`spam`) or "Pas spam": the filter learns it, the senders
    /// said "Pas spam" are never filtered again, and the messages move
    /// to the spam folder or back to the inbox.
    pub(super) fn mark_spam(&mut self, spam: bool) -> Task<Message> {
        let rows = self.action_rows();
        if rows.is_empty() {
            return Task::none();
        }
        if let Err(e) = self.store.learn_spam(&rows, spam) {
            return self.toast(format!("Erreur de la base locale : {e}"), true);
        }
        if !spam {
            for addr in self.store.senders_of(&rows).unwrap_or_default() {
                let _ = self.store.mark_not_spam(&addr);
            }
        }
        let to = if spam {
            Destination::Junk
        } else {
            Destination::Inbox
        };
        match self.move_rows(&rows, to) {
            Some(task) => {
                let toast = self.toast(super::destination_label(to), false);
                Task::batch([task, toast])
            }
            None if spam && !self.selection_in_junk() => self.toast(
                "Noté comme spam ; aucun dossier spam sur ce compte pour l'y ranger",
                false,
            ),
            None if spam => self.toast("Noté comme spam", false),
            None => self.toast("Noté : ce n'est pas du spam", false),
        }
    }

    /// The spam filter switched on or off in the settings.
    pub(super) fn spam_filter_toggled(&mut self, on: bool) -> Task<Message> {
        self.settings.spam_filter = on;
        self.save_settings();
        if !on {
            return Task::none();
        }
        let ids: Vec<AccountId> = self.accounts.iter().map(|a| a.id).collect();
        Task::batch(ids.into_iter().map(|id| self.learn_from_folders(id)))
    }

    /// Once per account, after a sync: the filter learns from what its
    /// spam folder and inbox already hold (off the UI thread, bounded).
    pub(super) fn learn_from_folders(&mut self, account: AccountId) -> Task<Message> {
        if !self.settings.spam_filter
            || self.learning.contains(&account)
            || !matches!(self.store.spam_bootstrapped(account), Ok(false))
            || !matches!(
                self.store.folder_by_role(account, FolderRole::Inbox),
                Ok(Some(_))
            )
        {
            return Task::none();
        }
        match &self.backend {
            Backend::Demo => {
                let _ = self.store.bootstrap_spam(account, BOOTSTRAP_LIMIT);
                Task::none()
            }
            Backend::Live { db_path, .. } => {
                self.learning.insert(account);
                let path = db_path.clone();
                Task::perform(bootstrap(path, account), move |result| {
                    Message::SpamLearned(account, result)
                })
            }
        }
    }

    pub(super) fn spam_learned(&mut self, account: AccountId, result: Result<usize, String>) {
        self.learning.remove(&account);
        if let Err(e) = result {
            eprintln!("Apprentissage de l'anti-spam impossible : {e}");
        }
    }
}

/// Applies one editing message to the rule form.
fn edit_form(form: &mut RuleForm, message: RuleMsg) {
    form.error = None;
    match message {
        RuleMsg::Name(name) => form.name = name,
        RuleMsg::Field(index, field) => {
            if let Some(condition) = form.conditions.get_mut(index) {
                condition.field = field;
            }
        }
        RuleMsg::Value(index, value) => {
            if let Some(condition) = form.conditions.get_mut(index) {
                condition.value = value;
            }
        }
        RuleMsg::AddCondition => form.conditions.push(Condition {
            field: RuleField::Subject,
            value: String::new(),
        }),
        RuleMsg::RemoveCondition(index) => {
            if form.conditions.len() > 1 && index < form.conditions.len() {
                form.conditions.remove(index);
            }
        }
        RuleMsg::MatchAll(all) => form.match_all = all,
        RuleMsg::MarkRead(on) => form.mark_read = on,
        RuleMsg::MoveTo(to) => form.move_to = to,
        RuleMsg::Scope(account) => form.account = account,
        _ => {}
    }
}

/// The one-time learning of an account, on a thread of its own with its
/// own connection to the database.
async fn bootstrap(path: PathBuf, account: AccountId) -> Result<usize, String> {
    let (sender, receiver) = oneshot::channel();
    std::thread::spawn(move || {
        let result = Store::open(&path)
            .and_then(|store| store.bootstrap_spam(account, BOOTSTRAP_LIMIT))
            .map_err(|e| e.to_string());
        let _ = sender.send(result);
    });
    receiver
        .await
        .unwrap_or_else(|_| Err("le calcul s'est arrêté".to_string()))
}

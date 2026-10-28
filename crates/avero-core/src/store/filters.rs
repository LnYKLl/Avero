//! The user's rules and the spam filter's model, in the database (see
//! `rules.rs` and `spam.rs` for what they mean).

use super::{Store, StoreError, filter_account, split_list};
use crate::category::sender_key;
use crate::rules::{Condition, Rule, RuleDef, RuleFacts, RuleField, RuleMove};
use crate::spam::{self, Learned, SpamInput, TokenCount};
use crate::{AccountFilter, AccountId, FolderId};
use rusqlite::{Connection, OptionalExtension, params};

/// Columns read by `facts_from_row`, on `m` (a message) and `f` (its
/// folder).
const FACT_COLUMNS: &str = "m.id, f.account_id, m.from_addr, m.from_name, m.to_addrs,
     m.delivered_to, m.subject, m.seen";

/// What the spam filter reads of a message: its key in `spam_learned`
/// and its tokens.
struct Learnable {
    key: String,
    tokens: Vec<String>,
}

impl Store {
    // ------------------------------------------------------------ rules

    /// Every rule, in the order they apply.
    pub fn rules(&self) -> Result<Vec<Rule>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, match_all, mark_read, move_to, account_id, enabled
             FROM rules ORDER BY position, id",
        )?;
        let rows = stmt.query_map([], |row| {
            let move_to: Option<String> = row.get(4)?;
            let account: Option<i64> = row.get(5)?;
            Ok(Rule {
                id: row.get(0)?,
                enabled: row.get(6)?,
                def: RuleDef {
                    name: row.get(1)?,
                    conditions: Vec::new(),
                    match_all: row.get(2)?,
                    mark_read: row.get(3)?,
                    move_to: move_to.as_deref().and_then(RuleMove::parse),
                    account: account.map(AccountId),
                },
            })
        })?;
        let mut rules: Vec<Rule> = rows.collect::<Result<_, _>>()?;
        let mut conditions = self.conn.prepare(
            "SELECT field, value FROM rule_conditions WHERE rule_id = ?1 ORDER BY position",
        )?;
        for rule in &mut rules {
            let found = conditions.query_map(params![rule.id], |row| {
                let field: String = row.get(0)?;
                Ok(Condition {
                    field: RuleField::parse(&field),
                    value: row.get(1)?,
                })
            })?;
            rule.def.conditions = found.collect::<Result<_, _>>()?;
        }
        Ok(rules)
    }

    /// Saves a new rule, enabled, after the others.
    pub fn add_rule(&self, def: &RuleDef) -> Result<Rule, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO rules (name, match_all, mark_read, move_to, account_id, enabled, position)
             VALUES (?1, ?2, ?3, ?4, ?5, 1,
                     (SELECT COALESCE(MAX(position), 0) + 1 FROM rules))",
            params![
                def.name,
                def.match_all,
                def.mark_read,
                def.move_to.map(|m| m.as_str()),
                def.account.map(|a| a.0),
            ],
        )?;
        let id = tx.last_insert_rowid();
        save_conditions(&tx, id, &def.conditions)?;
        tx.commit()?;
        Ok(Rule {
            id,
            enabled: true,
            def: def.clone(),
        })
    }

    /// Replaces what a rule says (its place and its switch stay).
    pub fn update_rule(&self, id: i64, def: &RuleDef) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE rules SET name = ?1, match_all = ?2, mark_read = ?3, move_to = ?4,
                 account_id = ?5
             WHERE id = ?6",
            params![
                def.name,
                def.match_all,
                def.mark_read,
                def.move_to.map(|m| m.as_str()),
                def.account.map(|a| a.0),
                id,
            ],
        )?;
        tx.execute(
            "DELETE FROM rule_conditions WHERE rule_id = ?1",
            params![id],
        )?;
        save_conditions(&tx, id, &def.conditions)?;
        tx.commit()?;
        Ok(())
    }

    /// Turns a rule on or off.
    pub fn set_rule_enabled(&self, id: i64, enabled: bool) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE rules SET enabled = ?1 WHERE id = ?2",
            params![enabled, id],
        )?;
        Ok(())
    }

    /// Deletes a rule (its conditions go with it).
    pub fn delete_rule(&self, id: i64) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM rules WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Moves a rule one place up (earlier) or down in the order.
    pub fn move_rule(&self, id: i64, up: bool) -> Result<(), StoreError> {
        let order: Vec<i64> = self.rules()?.into_iter().map(|r| r.id).collect();
        let Some(at) = order.iter().position(|&r| r == id) else {
            return Ok(());
        };
        let other = if up {
            at.checked_sub(1)
        } else {
            Some(at + 1).filter(|&i| i < order.len())
        };
        let Some(other) = other else {
            return Ok(());
        };
        let mut order = order;
        order.swap(at, other);
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare("UPDATE rules SET position = ?1 WHERE id = ?2")?;
            for (position, rule) in order.iter().enumerate() {
                stmt.execute(params![position as i64 + 1, rule])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// What the rules see of the inbox messages of `filter`'s accounts
    /// (hidden ones left out), newest first.
    pub fn inbox_facts(&self, filter: AccountFilter) -> Result<Vec<RuleFacts>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FACT_COLUMNS}
             FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE f.role = 'inbox' AND m.hidden = 0
               AND (?1 IS NULL OR f.account_id = ?1)
             ORDER BY m.date DESC, m.id DESC"
        ))?;
        let rows = stmt.query_map(params![filter_account(filter)], facts_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// What the rules see of the inbox messages of `account` with these
    /// UIDs (the new mail of a sync), in the order asked. Unknown or
    /// hidden ones are left out.
    pub fn arrival_facts(
        &self,
        account: AccountId,
        uids: &[u32],
    ) -> Result<Vec<RuleFacts>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FACT_COLUMNS}
             FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE f.role = 'inbox' AND f.account_id = ?1 AND m.uid = ?2 AND m.hidden = 0"
        ))?;
        let mut facts = Vec::new();
        for &uid in uids {
            let found = stmt
                .query_row(params![account.0, uid], facts_from_row)
                .optional()?;
            facts.extend(found);
        }
        Ok(facts)
    }

    // ------------------------------------------------------- spam filter

    /// Teaches the filter that these messages are spam (or not). Copies
    /// of one message count once; a message already learned the same way
    /// is skipped, and one learned the other way is taken back first.
    /// Returns how many messages changed the model.
    pub fn learn_spam(&self, rows: &[i64], spam: bool) -> Result<usize, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let mut changed = 0;
        for &row in rows {
            if let Some(message) = learnable(&tx, row)?
                && learn(&tx, &message, spam)?
            {
                changed += 1;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// How many messages the filter learned of each kind.
    pub fn spam_learned(&self) -> Result<Learned, StoreError> {
        learned(&self.conn)
    }

    /// The probability that a stored message is spam; `None` while the
    /// filter has not learned enough (see `spam::probability`).
    pub fn spam_probability(&self, row: i64) -> Result<Option<f64>, StoreError> {
        let Some(message) = learnable(&self.conn, row)? else {
            return Ok(None);
        };
        let learned = learned(&self.conn)?;
        if !learned.is_enough() {
            return Ok(None);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT spam, ham FROM spam_tokens WHERE token = ?1")?;
        let mut counts = Vec::new();
        for token in &message.tokens {
            let found = stmt
                .query_row(params![token], |row| {
                    Ok(TokenCount {
                        spam: row.get::<_, i64>(0)?.max(0) as u64,
                        ham: row.get::<_, i64>(1)?.max(0) as u64,
                    })
                })
                .optional()?;
            counts.extend(found);
        }
        Ok(spam::probability(&counts, learned))
    }

    /// Remembers that the user said "Pas spam" for this sender.
    pub fn mark_not_spam(&self, addr: &str) -> Result<(), StoreError> {
        let key = sender_key(addr);
        if key.is_empty() {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO senders (addr, not_spam) VALUES (?1, 1)
             ON CONFLICT(addr) DO UPDATE SET not_spam = 1",
            params![key],
        )?;
        Ok(())
    }

    /// Whether the spam filter must leave this sender's mail alone: the
    /// user said "Pas spam" for them, wrote to them (a message of a
    /// "sent" folder is addressed to them), or it is one of the user's
    /// own addresses.
    pub fn spam_exempt(&self, addr: &str) -> Result<bool, StoreError> {
        let key = sender_key(addr);
        if key.is_empty() {
            return Ok(false);
        }
        let exempt: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM senders WHERE addr = ?1 AND not_spam = 1)
                 OR EXISTS (SELECT 1 FROM accounts WHERE lower(trim(email)) = ?1)
                 OR EXISTS (
                     SELECT 1 FROM messages m JOIN folders f ON f.id = m.folder_id
                     WHERE f.role = 'sent'
                       AND instr(', ' || lower(m.to_addrs) || ',', ', ' || ?1 || ',') > 0
                 )",
            params![key],
            |row| row.get(0),
        )?;
        Ok(exempt)
    }

    /// Whether the filter already learned from what this account's spam
    /// and inbox folders held (see `bootstrap_spam`).
    pub fn spam_bootstrapped(&self, account: AccountId) -> Result<bool, StoreError> {
        let done: Option<bool> = self
            .conn
            .query_row(
                "SELECT spam_bootstrapped FROM accounts WHERE id = ?1",
                params![account.0],
                |row| row.get(0),
            )
            .optional()?;
        Ok(done.unwrap_or(true))
    }

    /// Learns, once per account, from the newest `limit` messages of its
    /// spam folder (as spam) and of its inbox (as wanted mail). Does
    /// nothing until the account has an inbox (before its first sync).
    /// Returns how many messages changed the model.
    pub fn bootstrap_spam(&self, account: AccountId, limit: usize) -> Result<usize, StoreError> {
        if self.spam_bootstrapped(account)? {
            return Ok(0);
        }
        let Some(inbox) = self.folder_by_role(account, crate::FolderRole::Inbox)? else {
            return Ok(0);
        };
        let junk = self.folder_by_role(account, crate::FolderRole::Junk)?;
        let newest = |folder: FolderId| -> Result<Vec<i64>, StoreError> {
            let mut stmt = self.conn.prepare(
                "SELECT id FROM messages WHERE folder_id = ?1 AND hidden = 0
                 ORDER BY date DESC, id DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![folder.0, limit as i64], |row| row.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        };
        let mut learned = 0;
        if let Some(junk) = junk {
            learned += self.learn_spam(&newest(junk.id)?, true)?;
        }
        // Mail the user already moved back or said "Pas spam" for stays
        // as they taught it: `learn` never overrides a message learned
        // the other way here.
        let ham: Vec<i64> = newest(inbox.id)?
            .into_iter()
            .filter(|&row| !matches!(learned_as(&self.conn, row), Ok(Some(true))))
            .collect();
        learned += self.learn_spam(&ham, false)?;
        self.conn.execute(
            "UPDATE accounts SET spam_bootstrapped = 1 WHERE id = ?1",
            params![account.0],
        )?;
        Ok(learned)
    }
}

/// Builds `RuleFacts` from a row of `FACT_COLUMNS`.
fn facts_from_row(row: &rusqlite::Row) -> rusqlite::Result<RuleFacts> {
    let to: Option<String> = row.get(4)?;
    let delivered: Option<String> = row.get(5)?;
    let mut to = split_list(to.as_deref(), ',');
    to.extend(delivered.filter(|d| !d.trim().is_empty()));
    Ok(RuleFacts {
        row: row.get(0)?,
        account: AccountId(row.get(1)?),
        from_addr: row.get(2)?,
        from_name: row.get(3)?,
        to,
        subject: row.get(6)?,
        seen: row.get(7)?,
    })
}

/// Writes the conditions of rule `id`, in order.
fn save_conditions(conn: &Connection, id: i64, conditions: &[Condition]) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(
        "INSERT INTO rule_conditions (rule_id, position, field, value) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (position, condition) in conditions.iter().enumerate() {
        stmt.execute(params![
            id,
            position as i64,
            condition.field.as_str(),
            condition.value.trim(),
        ])?;
    }
    Ok(())
}

/// The key and tokens of a stored message (its cached body included).
fn learnable(conn: &Connection, row: i64) -> Result<Option<Learnable>, StoreError> {
    let found = conn
        .query_row(
            "SELECT COALESCE(m.message_id, 'row:' || m.id), m.from_addr, m.from_name, m.subject,
                    substr(b.text, 1, 4000)
             FROM messages m LEFT JOIN bodies b ON b.message_row = m.id
             WHERE m.id = ?1",
            params![row],
            |r| {
                let from_addr: String = r.get(1)?;
                let from_name: Option<String> = r.get(2)?;
                let subject: String = r.get(3)?;
                let body: Option<String> = r.get(4)?;
                let tokens = spam::tokens(&SpamInput {
                    from_addr: &from_addr,
                    from_name: from_name.as_deref(),
                    subject: &subject,
                    body: body.as_deref(),
                });
                Ok(Learnable {
                    key: r.get(0)?,
                    tokens,
                })
            },
        )
        .optional()?;
    Ok(found)
}

/// How a stored message was learned, if it was (`Some(true)`: spam).
fn learned_as(conn: &Connection, row: i64) -> Result<Option<bool>, StoreError> {
    let verdict: Option<bool> = conn
        .query_row(
            "SELECT l.spam FROM spam_learned l JOIN messages m
                 ON l.key = COALESCE(m.message_id, 'row:' || m.id)
             WHERE m.id = ?1",
            params![row],
            |r| r.get(0),
        )
        .optional()?;
    Ok(verdict)
}

/// Adds one message to the model (inside the caller's transaction).
/// Returns whether the model changed.
fn learn(conn: &Connection, message: &Learnable, spam: bool) -> Result<bool, StoreError> {
    let before: Option<bool> = conn
        .query_row(
            "SELECT spam FROM spam_learned WHERE key = ?1",
            params![message.key],
            |row| row.get(0),
        )
        .optional()?;
    if before == Some(spam) {
        return Ok(false);
    }
    if before.is_some() {
        // Learned the other way: take that back first.
        let column = if spam { "ham" } else { "spam" };
        let mut undo = conn.prepare(&format!(
            "UPDATE spam_tokens SET {column} = MAX({column} - 1, 0) WHERE token = ?1"
        ))?;
        for token in &message.tokens {
            undo.execute(params![token])?;
        }
    }
    let column = if spam { "spam" } else { "ham" };
    let mut add = conn.prepare(&format!(
        "INSERT INTO spam_tokens (token, {column}) VALUES (?1, 1)
         ON CONFLICT(token) DO UPDATE SET {column} = {column} + 1"
    ))?;
    for token in &message.tokens {
        add.execute(params![token])?;
    }
    conn.execute(
        "INSERT INTO spam_learned (key, spam) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET spam = excluded.spam",
        params![message.key, spam],
    )?;
    Ok(true)
}

/// How many messages were learned of each kind.
fn learned(conn: &Connection) -> Result<Learned, StoreError> {
    let (spam, ham): (i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(spam), 0), COALESCE(SUM(1 - spam), 0) FROM spam_learned",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(Learned {
        spam: spam.max(0) as u64,
        ham: ham.max(0) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{acct, folder, inbox, msg};
    use super::*;
    use crate::rules::{self, block_rule};
    use crate::{FolderRole, NewMessage};

    fn sample(uid: u32, from: &str, subject: &str) -> NewMessage {
        let mut m = msg(uid, Some(&format!("<m{uid}@test>")), 1000 + uid as i64);
        m.from_addr = from.to_string();
        m.subject = subject.to_string();
        m
    }

    fn row_of(store: &Store, folder: FolderId, uid: u32) -> i64 {
        store
            .conn
            .query_row(
                "SELECT id FROM messages WHERE folder_id = ?1 AND uid = ?2",
                params![folder.0, uid],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn rules_are_saved_in_order_and_edited() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "moi@x.fr");
        let first = s.add_rule(&block_rule("promo@shop.fr", "")).unwrap();
        let second = s
            .add_rule(&RuleDef {
                name: "Factures".into(),
                conditions: vec![
                    Condition {
                        field: RuleField::Subject,
                        value: " facture ".into(),
                    },
                    Condition {
                        field: RuleField::To,
                        value: "compta@".into(),
                    },
                ],
                match_all: false,
                mark_read: true,
                move_to: Some(RuleMove::Archive),
                account: Some(a),
            })
            .unwrap();
        let all = s.rules().unwrap();
        assert_eq!(
            all.iter().map(|r| r.id).collect::<Vec<_>>(),
            [first.id, second.id]
        );
        assert_eq!(all[1].def.conditions[0].value, "facture", "trimmed");
        assert_eq!(all[1].def.account, Some(a));
        assert_eq!(all[1].def.move_to, Some(RuleMove::Archive));
        assert!(all[1].def.mark_read && !all[1].def.match_all);

        s.move_rule(second.id, true).unwrap();
        let order: Vec<i64> = s.rules().unwrap().iter().map(|r| r.id).collect();
        assert_eq!(order, [second.id, first.id]);
        s.move_rule(second.id, true).unwrap(); // already first
        s.set_rule_enabled(first.id, false).unwrap();
        let mut def = first.def.clone();
        def.name = "Plus de promo".into();
        s.update_rule(first.id, &def).unwrap();
        let edited = s.rules().unwrap().pop().unwrap();
        assert_eq!(edited.def.name, "Plus de promo");
        assert!(!edited.enabled, "update keeps the switch");
        assert_eq!(edited.def.conditions.len(), 1);

        s.delete_rule(second.id).unwrap();
        assert_eq!(s.rules().unwrap().len(), 1);
        assert_eq!(
            super::super::tests::count(&s, "rule_conditions"),
            1,
            "conditions go with their rule"
        );
    }

    #[test]
    fn rules_of_a_deleted_account_go_with_it() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "moi@x.fr");
        let mut def = block_rule("promo@shop.fr", "");
        def.account = Some(a);
        s.add_rule(&def).unwrap();
        s.add_rule(&block_rule("autre@shop.fr", "")).unwrap();
        s.delete_account(a).unwrap();
        assert_eq!(s.rules().unwrap().len(), 1);
    }

    #[test]
    fn facts_of_the_inbox_and_of_arrivals() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "moi@x.fr");
        let b = acct(&s, "autre@x.fr");
        let box_a = inbox(&s, a).id;
        let box_b = inbox(&s, b).id;
        let archive = folder(&s, a, "Archive", FolderRole::Archive);
        let mut m = sample(1, "promo@shop.fr", "Soldes");
        m.headers.to = vec!["moi@x.fr".into(), "liste@x.fr".into()];
        m.headers.delivered_to = Some("moi+promo@x.fr".into());
        s.insert_messages(box_a, &[m, sample(2, "ami@x.fr", "Salut")])
            .unwrap();
        s.insert_messages(box_b, &[sample(3, "promo@shop.fr", "Soldes")])
            .unwrap();
        s.insert_messages(archive, &[sample(4, "promo@shop.fr", "Vieux")])
            .unwrap();

        let all = s.inbox_facts(AccountFilter::All).unwrap();
        assert_eq!(all.len(), 3, "the archive is left out");
        let only_a = s.inbox_facts(AccountFilter::Only(a)).unwrap();
        assert_eq!(only_a.len(), 2);
        let arrivals = s.arrival_facts(a, &[1, 3, 9]).unwrap();
        assert_eq!(arrivals.len(), 1, "uid 3 is another account's");
        assert_eq!(arrivals[0].to, ["moi@x.fr", "liste@x.fr", "moi+promo@x.fr"]);
        assert_eq!(arrivals[0].account, a);

        // The rules see them.
        let rule = Rule {
            id: 1,
            enabled: true,
            def: block_rule("shop.fr", ""),
        };
        let caught: Vec<i64> = all
            .iter()
            .filter(|f| {
                rules::outcome(std::slice::from_ref(&rule), f)
                    .move_to
                    .is_some()
            })
            .map(|f| f.row)
            .collect();
        assert_eq!(caught.len(), 2);
    }

    /// Learns `n` spam messages (casino) and `n` wanted ones (meetings).
    fn trained(s: &Store, n: u32) -> FolderId {
        let a = acct(s, "moi@x.fr");
        let box_a = inbox(s, a).id;
        let junk = folder(s, a, "Spam", FolderRole::Junk);
        let mut spam_rows = Vec::new();
        let mut ham_rows = Vec::new();
        for i in 0..n {
            let uid = 100 + i;
            s.insert_messages(
                junk,
                &[sample(
                    uid,
                    "win@casino-gold.biz",
                    "Gagnez gros au casino bonus",
                )],
            )
            .unwrap();
            spam_rows.push(row_of(s, junk, uid));
            let uid = 500 + i;
            s.insert_messages(
                box_a,
                &[sample(uid, "lea@atelier.fr", "Réunion projet planning")],
            )
            .unwrap();
            ham_rows.push(row_of(s, box_a, uid));
        }
        assert_eq!(s.learn_spam(&spam_rows, true).unwrap(), n as usize);
        assert_eq!(s.learn_spam(&ham_rows, false).unwrap(), n as usize);
        box_a
    }

    #[test]
    fn learning_then_classifying() {
        let s = Store::open_in_memory().unwrap();
        let box_a = trained(&s, 25);
        assert_eq!(s.spam_learned().unwrap(), Learned { spam: 25, ham: 25 });
        s.insert_messages(
            box_a,
            &[
                sample(1, "win@casino-gold.biz", "Bonus casino : gagnez"),
                sample(2, "lea@atelier.fr", "Planning de la réunion"),
            ],
        )
        .unwrap();
        let spam = s.spam_probability(row_of(&s, box_a, 1)).unwrap().unwrap();
        let ham = s.spam_probability(row_of(&s, box_a, 2)).unwrap().unwrap();
        assert!(spam::is_spam(Some(spam)), "{spam}");
        assert!(ham < 0.01, "{ham}");
    }

    #[test]
    fn too_little_learning_classifies_nothing() {
        let s = Store::open_in_memory().unwrap();
        let box_a = trained(&s, 5);
        s.insert_messages(box_a, &[sample(1, "win@casino-gold.biz", "casino bonus")])
            .unwrap();
        assert_eq!(s.spam_probability(row_of(&s, box_a, 1)).unwrap(), None);
    }

    #[test]
    fn learning_twice_counts_once_and_the_other_way_takes_back() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "moi@x.fr");
        let box_a = inbox(&s, a).id;
        s.insert_messages(box_a, &[sample(1, "x@promo.fr", "Promo")])
            .unwrap();
        let row = row_of(&s, box_a, 1);
        assert_eq!(s.learn_spam(&[row, row], true).unwrap(), 1);
        assert_eq!(s.spam_learned().unwrap(), Learned { spam: 1, ham: 0 });
        let token = |name: &str| -> (i64, i64) {
            s.conn
                .query_row(
                    "SELECT spam, ham FROM spam_tokens WHERE token = ?1",
                    params![name],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap()
        };
        assert_eq!(token("s:promo"), (1, 0));
        // "Pas spam": back to ham.
        assert_eq!(s.learn_spam(&[row], false).unwrap(), 1);
        assert_eq!(s.spam_learned().unwrap(), Learned { spam: 0, ham: 1 });
        assert_eq!(token("s:promo"), (0, 1));
        // Spam again: the same message, back and forth.
        assert_eq!(s.learn_spam(&[row], true).unwrap(), 1);
        assert_eq!(token("s:promo"), (1, 0));
    }

    #[test]
    fn exempt_senders() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "moi@x.fr");
        let sent = folder(&s, a, "Sent", FolderRole::Sent);
        let mut mine = sample(1, "moi@x.fr", "Re: devis");
        mine.headers.to = vec!["client@pme.fr".into(), "bob@pme.fr".into()];
        s.insert_messages(sent, &[mine]).unwrap();
        assert!(s.spam_exempt("Client@PME.fr").unwrap(), "written to");
        assert!(s.spam_exempt("bob@pme.fr").unwrap());
        assert!(
            !s.spam_exempt("ent@pme.fr").unwrap(),
            "whole addresses only"
        );
        assert!(s.spam_exempt("MOI@x.fr").unwrap(), "own address");
        assert!(!s.spam_exempt("news@promo.fr").unwrap());
        s.mark_not_spam(" News@Promo.fr").unwrap();
        assert!(s.spam_exempt("news@promo.fr").unwrap(), "said not spam");
    }

    #[test]
    fn bootstrap_learns_once_after_the_first_sync() {
        let s = Store::open_in_memory().unwrap();
        let a = acct(&s, "moi@x.fr");
        assert!(!s.spam_bootstrapped(a).unwrap());
        assert_eq!(s.bootstrap_spam(a, 10).unwrap(), 0);
        assert!(!s.spam_bootstrapped(a).unwrap(), "no inbox yet");

        let box_a = inbox(&s, a).id;
        let junk = folder(&s, a, "Spam", FolderRole::Junk);
        for uid in 1..=4 {
            s.insert_messages(junk, &[sample(uid, "x@casino.biz", "casino")])
                .unwrap();
        }
        for uid in 11..=13 {
            s.insert_messages(box_a, &[sample(uid, "lea@atelier.fr", "projet")])
                .unwrap();
        }
        // Bounded: the 3 newest of each folder.
        assert_eq!(s.bootstrap_spam(a, 3).unwrap(), 6);
        assert_eq!(s.spam_learned().unwrap(), Learned { spam: 3, ham: 3 });
        assert!(s.spam_bootstrapped(a).unwrap());
        assert_eq!(s.bootstrap_spam(a, 3).unwrap(), 0, "only once");
    }
}

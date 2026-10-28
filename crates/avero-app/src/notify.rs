//! System notifications for new mail. They show the sender and the
//! subject, nothing else of the message.

use avero_sync::sync::Arrival;

/// Above this many new messages at once, one notification counts them
/// instead of one each.
pub const GROUP_ABOVE: usize = 3;

/// One notification: a title and a line of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
}

/// The notifications for the new messages of one account: one per
/// message (sender, then subject), or a single « 12 nouveaux messages »
/// when there are many.
pub fn notices(arrivals: &[Arrival], account_name: &str) -> Vec<Notice> {
    if arrivals.len() > GROUP_ABOVE {
        return vec![Notice {
            title: format!("{} nouveaux messages", arrivals.len()),
            body: account_name.to_string(),
        }];
    }
    arrivals
        .iter()
        .map(|arrival| Notice {
            title: sender(arrival),
            body: match arrival.subject.trim() {
                "" => "(sans objet)".to_string(),
                subject => subject.to_string(),
            },
        })
        .collect()
}

/// The sender's name, else its address.
fn sender(arrival: &Arrival) -> String {
    match arrival.from_name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => arrival.from_addr.clone(),
    }
}

/// Shows the notifications from a thread of their own, never from the
/// interface's. A system that refuses them (or a backend that panics)
/// only leaves a line in the log, without the message's details.
pub fn show(notices: Vec<Notice>) {
    if notices.is_empty() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("avero-notify".to_string())
        .spawn(move || {
            for notice in notices {
                let shown = std::panic::catch_unwind(|| {
                    notify_rust::Notification::new()
                        .appname("Avero")
                        .summary(&notice.title)
                        .body(&notice.body)
                        .show()
                        .map(drop)
                });
                match shown {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => eprintln!("Notification refusée par le système : {e}"),
                    Err(_) => eprintln!("Notification impossible : erreur du système"),
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("Notification impossible : {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arrival(uid: u32, name: Option<&str>, subject: &str) -> Arrival {
        Arrival {
            uid,
            from_name: name.map(String::from),
            from_addr: format!("n{uid}@exemple.fr"),
            subject: subject.to_string(),
        }
    }

    #[test]
    fn few_messages_get_one_notice_each() {
        let notices = notices(
            &[
                arrival(1, Some("Alice Martin"), "Réunion"),
                arrival(2, None, "  "),
            ],
            "Perso",
        );
        assert_eq!(
            notices,
            vec![
                Notice {
                    title: "Alice Martin".into(),
                    body: "Réunion".into(),
                },
                Notice {
                    title: "n2@exemple.fr".into(),
                    body: "(sans objet)".into(),
                },
            ]
        );
    }

    #[test]
    fn many_messages_are_grouped() {
        let arrivals: Vec<Arrival> = (1..=12).map(|uid| arrival(uid, None, "s")).collect();
        let notices = notices(&arrivals, "Perso");
        assert_eq!(
            notices,
            vec![Notice {
                title: "12 nouveaux messages".into(),
                body: "Perso".into(),
            }]
        );
        // Right at the limit, still one each.
        assert_eq!(super::notices(&arrivals[..GROUP_ABOVE], "Perso").len(), 3);
    }

    #[test]
    fn nothing_new_shows_nothing() {
        assert!(notices(&[], "Perso").is_empty());
    }
}

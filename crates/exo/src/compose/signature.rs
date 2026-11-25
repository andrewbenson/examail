//! The From alias's signature in a draft, which is Gmail's `sendAs` signature for the alias. It
//! sits behind the draft's `…` until it's revealed into the body, and is appended at send if it
//! never was. A reply or forward carries the signature's content only while
//! `Include signature on replies and forwards` is on.

use exo_store::{Address, Alias};

use super::draft::Open;

/// The Gmail signature of the alias that `from` is, sanitized and without the
/// `<div dir="ltr"><br><div>` shell Gmail wraps signatures in. `None` when the alias has no
/// signature, or `from` isn't an alias.
pub(crate) fn of(from: Option<&Address>, aliases: &[Alias]) -> Option<String> {
    let from = from?;
    let alias = aliases
        .iter()
        .find(|alias| alias.email.eq_ignore_ascii_case(&from.email))?;
    let signature = exo_mail::sanitize_fragment(alias.signature.as_deref()?);
    let trimmed = signature.trim();
    let shell = trimmed
        .strip_prefix("<div dir=\"ltr\"><br><div>")
        .and_then(|inner| inner.strip_suffix("</div></div>"));
    let signature = shell.unwrap_or(trimmed).to_owned();
    (!signature.is_empty()).then_some(signature)
}

/// Whether `open` carries its signature's content. New mail always does, and a reply or forward
/// does while the setting is on.
pub(crate) fn includes_content(open: &Open) -> bool {
    open.is_new_email() || open.body.signature_on_replies
}

/// The block that `…` reveals into the body under what's written. It holds the alias's
/// signature, with its content as [`includes_content`] says, or is an empty block for an alias
/// without one, which choosing another From fills ([`switch`]).
pub(crate) fn revealed(open: &Open, aliases: &[Alias]) -> String {
    exo_editor::outgoing::signature(
        of(open.record.from.as_ref(), aliases).as_deref(),
        includes_content(open),
        Some(&open.id),
    )
}

/// Once From changed from `old` with the signature already in the body, replaces the block with
/// the new alias's signature. The block stays when its text is no longer the old alias's
/// signature, because it was edited or the quote was deleted. Both signatures are rendered with
/// their content whatever the setting, so a block revealed empty takes the new alias's whole
/// signature.
pub(crate) fn switch(open: &mut Open, aliases: &[Alias], old: Option<&Address>) {
    let block = |from: Option<&Address>| {
        exo_editor::outgoing::signature(of(from, aliases).as_deref(), true, Some(&open.id))
    };
    let (old, new) = (block(old), block(open.record.from.as_ref()));
    open.editor.switch_signature(&open.id, &old, &new);
}

#[cfg(test)]
mod tests {
    use super::*;
    use exo_store::{DraftAction, DraftRecord};

    fn aliases() -> Vec<Alias> {
        let alias = |email: &str, signature: Option<&str>, default| Alias {
            email: email.into(),
            name: Some("Maya Okafor".into()),
            signature: signature.map(Into::into),
            default,
            primary: default,
            active: true,
        };
        vec![
            alias(
                "maya@brightmoor.co",
                Some(
                    "<div dir=\"ltr\"><div><b>Maya Okafor</b></div><div>Head of Product</div></div>",
                ),
                true,
            ),
            alias(
                "hello@mayaokafor.com",
                Some("<div dir=\"ltr\">Maya</div>"),
                false,
            ),
            alias("bare@mayaokafor.com", None, false),
        ]
    }

    fn from(email: &str) -> Option<Address> {
        Some(Address {
            name: None,
            email: email.into(),
        })
    }

    fn open(action: DraftAction, email: &str) -> Open {
        let record = DraftRecord {
            action,
            from: from(email),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::new(),
            body: String::new(),
            quoted_content: (action != DraftAction::Compose).then(|| "<div class=\"gmail_quote\">On Tue, Tom wrote:<blockquote class=\"gmail_quote\"><div>Heads up</div></blockquote></div>".into()),
            quoted_inlined: false,
            quote_signed: false,
            in_reply_to: None,
            in_reply_to_rfc822: None,
            references: Vec::new(),
            rfc822_id: "c.d@brightmoor.co".into(),
            fingerprint: String::new(),
            attachments: Vec::new(),
            send_later: None,
            reminder: None,
        };
        let mut open = Open::new("draft00aaaaaaaaaaaaaa".into(), None, record);
        open.editor.type_text("Sounds good");
        open
    }

    #[test]
    fn signatures_lose_gmails_shell() {
        let shelled = vec![Alias {
            signature: Some("<div dir=\"ltr\"><br><div><b>Maya</b></div></div>".into()),
            ..aliases()[0].clone()
        }];
        assert_eq!(
            of(from("Maya@Brightmoor.co").as_ref(), &shelled).as_deref(),
            Some("<b>Maya</b>")
        );
        assert_eq!(
            of(from("hello@mayaokafor.com").as_ref(), &aliases()).as_deref(),
            Some("<div dir=\"ltr\">Maya</div>")
        );
        assert_eq!(of(from("bare@mayaokafor.com").as_ref(), &aliases()), None);
        assert_eq!(of(from("other@x.com").as_ref(), &aliases()), None);
    }

    #[test]
    fn replies_carry_the_content_only_with_the_setting() {
        let mut reply = open(DraftAction::Reply, "maya@brightmoor.co");
        assert!(includes_content(&reply), "on by default");
        reply.body.signature_on_replies = false;
        assert!(!includes_content(&reply));
        assert_eq!(
            revealed(&reply, &aliases()),
            "<div class=\"gmail_signature\" data-signature-draft-id=\"draft00aaaaaaaaaaaaaa\"><br/></div>"
        );
        let mut new = open(DraftAction::Compose, "maya@brightmoor.co");
        new.body.signature_on_replies = false;
        assert!(includes_content(&new), "new mail always does");
        let mut forward = open(DraftAction::Forward, "maya@brightmoor.co");
        forward.body.signature_on_replies = false;
        assert!(!includes_content(&forward));
    }

    #[test]
    fn changing_from_swaps_an_untouched_signature() {
        let aliases = aliases();
        let mut reply = open(DraftAction::Reply, "maya@brightmoor.co");
        super::super::form::reveal_quote(&mut reply, &aliases, 0.0);
        assert!(
            reply.editor.html().contains("Head of Product"),
            "{}",
            reply.editor.html()
        );
        let old = reply
            .record
            .from
            .replace(from("hello@mayaokafor.com").expect("an address"));
        switch(&mut reply, &aliases, old.as_ref());
        let html = reply.editor.html();
        assert!(
            !html.contains("Head of Product") && html.contains(">Maya<"),
            "{html}"
        );
        assert!(
            html.contains("Sounds good") && html.contains("Heads up"),
            "{html}"
        );
    }

    #[test]
    fn an_alias_without_one_leaves_a_block_another_fills() {
        let aliases = aliases();
        let mut reply = open(DraftAction::Reply, "bare@mayaokafor.com");
        super::super::form::reveal_quote(&mut reply, &aliases, 0.0);
        assert!(
            reply.editor.html().contains("data-signature-draft-id"),
            "{}",
            reply.editor.html()
        );
        let old = reply
            .record
            .from
            .replace(from("maya@brightmoor.co").expect("an address"));
        switch(&mut reply, &aliases, old.as_ref());
        assert!(
            reply.editor.html().contains("Head of Product"),
            "{}",
            reply.editor.html()
        );
    }
}

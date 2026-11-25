//! Thread pipeline rendering, reply-parent resolution, and search-index visible text extraction.

use std::collections::HashMap;

use exo_mail::Render;
use exo_store::MessageDetail;

use super::Facts;

/// Resolves which earlier message in `rfc822_ids[..at]` a message at `at` replies to: the one its
/// `in_reply_to` names, or else the immediately preceding message.
pub(super) fn reply_parent<'a>(
    at: usize,
    in_reply_to: Option<&str>,
    mut earlier_rfc822_ids: impl Iterator<Item = Option<&'a str>>,
) -> Option<usize> {
    in_reply_to
        .and_then(|parent| earlier_rfc822_ids.position(|id| id == Some(parent)))
        .or(at.checked_sub(1))
}

/// Runs the pipeline on every message, oldest first, each against the one it replies to, which is
/// the message its `In-Reply-To` names or else the one before it.
pub(super) fn render_thread(
    sources: &[MessageDetail],
    own_domains: &[String],
    options: exo_mail::RenderOptions,
) -> HashMap<String, Render> {
    let messages: Vec<exo_mail::Message> = sources
        .iter()
        .map(|source| exo_mail::Message {
            html: body_html(&source.body),
            from_email: source
                .from
                .as_ref()
                .map(|from| from.email.clone())
                .unwrap_or_default(),
            from_name: source
                .from
                .as_ref()
                .and_then(|from| from.name.clone())
                .unwrap_or_default(),
            rfc822_id: source.rfc822_id.clone().unwrap_or_default(),
            attachment_names: source
                .attachments
                .iter()
                .filter(|attachment| !attachment.inline)
                .map(|attachment| attachment.filename.clone())
                .collect(),
        })
        .collect();
    let mut renders: Vec<Render> = Vec::with_capacity(messages.len());
    for (at, message) in messages.iter().enumerate() {
        let parent = reply_parent(
            at,
            sources[at].in_reply_to.as_deref(),
            sources[..at]
                .iter()
                .map(|earlier| earlier.rfc822_id.as_deref()),
        );
        let replying_to = parent.map(|parent| (&messages[parent], &renders[parent]));
        renders.push(exo_mail::render_with(
            message,
            replying_to,
            own_domains,
            options,
        ));
    }
    sources
        .iter()
        .map(|source| source.id.clone())
        .zip(renders)
        .collect()
}

/// The text a reader sees in each of `sources`, each read against the one it replies to as
/// [`render_thread`] reads them. This is what the search index holds (`Store::index_text_with`).
pub(crate) fn visible_texts(sources: &[exo_store::TextSource]) -> Vec<String> {
    let messages: Vec<exo_mail::Message> = sources
        .iter()
        .map(|source| exo_mail::Message {
            html: body_html(&source.body),
            from_email: source.from_email.clone(),
            from_name: source.from_name.clone(),
            rfc822_id: source.rfc822_id.clone(),
            attachment_names: source.attachment_names.clone(),
        })
        .collect();
    let replying_to: Vec<Option<usize>> = sources
        .iter()
        .enumerate()
        .map(|(at, source)| source.replying_to.filter(|&parent| parent < at))
        .collect();
    exo_mail::visible_texts(&messages, &replying_to, &[])
}

/// A body as the pipeline takes it, which is its HTML or its plain text as HTML.
fn body_html(body: &exo_store::Body) -> String {
    match (&body.html, &body.text) {
        (Some(html), _) => html.clone(),
        (None, Some(text)) => exo_mail::text_to_html(text),
        (None, None) => String::new(),
    }
}

pub(super) fn facts(render: &Render) -> Facts {
    Facts {
        snippet: render.snippet.clone(),
        visible_chars: render.visible_text.chars().count(),
        has_quoted: render.has_quoted_content,
        unquoted_marketing: render.unquoted_is_marketing,
        quoted_marketing: render.quoted_is_marketing,
        tracking_blocked: !render.read_status_urls.is_empty(),
        remote_images: render.has_remote_images,
        unsubscribe: render
            .unsubscribe_link
            .as_ref()
            .map(|link| link.url.clone()),
        unquoted_html: render.unquoted_html.clone(),
        quoted_html: render.quoted_html.clone(),
        html: render.html.clone(),
        body_attributes: render.body_attributes.clone(),
        links: render.links.clone(),
        actions: render.actions.clone(),
        calendar: render.calendar_data.clone(),
        unquoted_dates: render.unquoted_dates.clone(),
        proposes_time: render.proposes_time,
    }
}

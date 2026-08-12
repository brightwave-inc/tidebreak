//! Shared HTML for the one-shot OAuth loopback callback pages.
//!
//! Gateway and ChatGPT sign-in both park an ephemeral listener that serves a
//! self-contained result page after the browser redirects back. The markup
//! must stay fully inline (no external assets) — the listener dies as soon as
//! the exchange finishes, and the page is only ever opened on loopback.

/// Kind of outcome the loopback page is reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallbackOutcome {
    Success,
    Denied,
    Failed,
}

/// Fully self-contained HTML for the browser tab the OAuth redirect lands on.
pub(crate) fn callback_page(outcome: CallbackOutcome, heading: &str, message: &str) -> String {
    let (status_label, icon_svg, tone) = match outcome {
        CallbackOutcome::Success => (
            "Connected",
            r#"<svg class="icon" viewBox="0 0 48 48" fill="none" aria-hidden="true"><circle cx="24" cy="24" r="24" fill="var(--tone-soft)"/><path d="M15 24.5 21.5 31 33 17.5" stroke="var(--tone)" stroke-width="3.2" stroke-linecap="round" stroke-linejoin="round"/></svg>"#,
            "success",
        ),
        CallbackOutcome::Denied => (
            "Not connected",
            r#"<svg class="icon" viewBox="0 0 48 48" fill="none" aria-hidden="true"><circle cx="24" cy="24" r="24" fill="var(--tone-soft)"/><path d="M17 17 31 31M31 17 17 31" stroke="var(--tone)" stroke-width="3.2" stroke-linecap="round"/></svg>"#,
            "danger",
        ),
        CallbackOutcome::Failed => (
            "Something went wrong",
            r#"<svg class="icon" viewBox="0 0 48 48" fill="none" aria-hidden="true"><circle cx="24" cy="24" r="24" fill="var(--tone-soft)"/><path d="M24 15v12" stroke="var(--tone)" stroke-width="3.2" stroke-linecap="round"/><circle cx="24" cy="33.5" r="1.8" fill="var(--tone)"/></svg>"#,
            "danger",
        ),
    };

    // Headings/messages are fixed literals from our call sites — still escape
    // so a future caller cannot break out of the markup.
    let heading = escape_html(heading);
    let message = escape_html(message);
    let message_html = if message.is_empty() {
        String::new()
    } else {
        format!(r#"<p class="message">{message}</p>"#)
    };

    format!(
        r#"<!doctype html>
<html lang="en" data-tone="{tone}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>{heading} · Tidebreak</title>
<style>
  :root {{
    color-scheme: light dark;
    --bg: #f4f4f5;
    --bg-accent: radial-gradient(1200px 600px at 50% -10%, #e4e4e7 0%, transparent 60%);
    --card: #ffffff;
    --border: #e4e4e7;
    --text: #18181b;
    --muted: #52525b;
    --tone: #15803d;
    --tone-soft: #dcfce7;
    --shadow: 0 1px 2px rgb(0 0 0 / 0.04), 0 12px 32px rgb(0 0 0 / 0.06);
  }}
  :root[data-tone="danger"] {{
    --tone: #b91c1c;
    --tone-soft: #fee2e2;
  }}
  @media (prefers-color-scheme: dark) {{
    :root {{
      --bg: #0c0c0e;
      --bg-accent: radial-gradient(1000px 520px at 50% -15%, #27272a 0%, transparent 55%);
      --card: #18181b;
      --border: #27272a;
      --text: #fafafa;
      --muted: #a1a1aa;
      --tone: #4ade80;
      --tone-soft: rgb(74 222 128 / 0.14);
      --shadow: 0 1px 2px rgb(0 0 0 / 0.4), 0 16px 40px rgb(0 0 0 / 0.45);
    }}
    :root[data-tone="danger"] {{
      --tone: #f87171;
      --tone-soft: rgb(248 113 113 / 0.14);
    }}
  }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0;
    min-height: 100vh;
    display: grid;
    place-items: center;
    padding: 1.5rem;
    background: var(--bg);
    background-image: var(--bg-accent);
    color: var(--text);
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto,
      "Helvetica Neue", Arial, sans-serif;
    -webkit-font-smoothing: antialiased;
  }}
  main {{
    width: min(100%, 22.5rem);
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 16px;
    padding: 2rem 1.75rem 1.75rem;
    text-align: center;
    box-shadow: var(--shadow);
  }}
  .brand {{
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    color: var(--muted);
    font-size: 0.8125rem;
    font-weight: 600;
    letter-spacing: 0.01em;
    margin: 0 0 1.5rem;
  }}
  .brand svg {{
    width: 1.15rem;
    height: auto;
    color: var(--text);
    opacity: 0.9;
  }}
  .icon {{
    width: 3rem;
    height: 3rem;
    margin: 0 auto 1.1rem;
    display: block;
  }}
  .status {{
    display: inline-block;
    margin: 0 0 0.65rem;
    padding: 0.2rem 0.55rem;
    border-radius: 999px;
    background: var(--tone-soft);
    color: var(--tone);
    font-size: 0.6875rem;
    font-weight: 650;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }}
  h1 {{
    font-size: 1.375rem;
    font-weight: 650;
    letter-spacing: -0.02em;
    line-height: 1.25;
    margin: 0 0 0.5rem;
  }}
  .message {{
    color: var(--muted);
    font-size: 0.9375rem;
    line-height: 1.55;
    margin: 0 0 1.35rem;
  }}
  .foot {{
    margin: 0;
    padding-top: 1rem;
    border-top: 1px solid var(--border);
    color: var(--muted);
    font-size: 0.8125rem;
    line-height: 1.45;
  }}
</style>
</head>
<body>
<main>
  <p class="brand">
    <svg xmlns="http://www.w3.org/2000/svg" width="24" height="13" viewBox="127 217 757 407" fill="currentColor" aria-hidden="true">
      <path d="M1736 3851l-179-178 4-9c2-5 306-309 675-676l671-668h723v18l-1 17-832 838-832 838-25-1-25-1-179-178zM6430 2939 5345 1860l546-540h999v25l-300 300c-165 165-300 303-300 307s87 93 193 198l192 191 305-6 1663 1663-14 22-1114-1-1085-1080zM3575 2718c208-211 400-404 427-430l48-48v-25l-450-450-755-4-1373-1386 9-15 1604 1 1875 1866v28l-845 845h-919l379-382zM6860 2005l-115-116 345-341c190-188 460-456 602-596l256-254 10 3c5 2 159 150 341 328l331 324v22l-745 745h-910l-115-115z" transform="translate(0 640) scale(.1 -.1)"/>
    </svg>
    Tidebreak
  </p>
  {icon}
  <div class="status">{status}</div>
  <h1>{heading}</h1>
  {message_html}
  <p class="foot">You can close this tab and return to the app.</p>
</main>
</body>
</html>"#,
        tone = tone,
        heading = heading,
        message_html = message_html,
        icon = icon_svg,
        status = status_label,
    )
}

fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_page_marks_connected_and_escapes() {
        let html = callback_page(
            CallbackOutcome::Success,
            r#"Signed in <script>"#,
            r#"a & b <c>"#,
        );
        assert!(html.contains("data-tone=\"success\""));
        assert!(html.contains("Connected"));
        assert!(html.contains("Signed in &lt;script&gt;"));
        assert!(html.contains("a &amp; b &lt;c&gt;"));
        assert!(html.contains(r#"class="message""#));
        assert!(!html.contains("<script>"));
        // Self-contained: no stylesheets, fonts, images, or scripts fetched
        // over the network. The SVG xmlns is an identifier, not a request.
        assert!(!html.contains(r#"href="http"#));
        assert!(!html.contains("href='http"));
        assert!(!html.contains(r#"src="http"#));
        assert!(!html.contains("src='http"));
        assert!(!html.contains("@import"));
        assert!(!html.contains("url(http"));

        let bare = callback_page(CallbackOutcome::Success, "You're signed in", "");
        assert!(
            !bare.contains(r#"class="message""#),
            "empty success body should omit the message paragraph"
        );
    }

    #[test]
    fn denied_page_uses_danger_tone() {
        let html = callback_page(CallbackOutcome::Denied, "Sign-in denied", "Nope.");
        assert!(html.contains("data-tone=\"danger\""));
        assert!(html.contains("Not connected"));
    }
}

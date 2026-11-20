//! The site in `build/`: the pages, the navigation between them and the look
//! they share. Pages other than the spending page are plain documents: a body
//! of HTML made elsewhere, put in [`document`]'s frame.

use std::path::Path;

use anyhow::{Context, Result};

/// The look every page shares: colours (light and dark), the header and the navigation.
pub const CSS: &str = include_str!("site.css");

const DOCUMENT: &str = include_str!("doc.html");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    /// The front page, leading to the others.
    Home,
    Spending,
    Income,
    Gaps,
}

impl Page {
    pub const ALL: [Page; 4] = [Page::Home, Page::Spending, Page::Income, Page::Gaps];

    /// The file in the site's directory.
    pub fn file(self) -> &'static str {
        match self {
            Page::Home => "index.html",
            Page::Spending => "spending.html",
            Page::Income => "income.html",
            Page::Gaps => "gaps.html",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Page::Home => "Books",
            Page::Spending => "Spending",
            Page::Income => "Income",
            Page::Gaps => "Gaps",
        }
    }
}

/// Text for HTML, with `&`, `<`, `>` and `"` escaped.
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Links to every page, marking the current one.
pub fn nav(current: Page) -> String {
    let links: String = Page::ALL
        .iter()
        .map(|&p| {
            let current = if p == current { " aria-current=\"page\"" } else { "" };
            format!("<a href=\"{}\"{current}>{}</a>", p.file(), p.title())
        })
        .collect();
    format!("<nav class=\"site\">{links}</nav>")
}

/// A page with a body of HTML. The subtitle is text.
pub fn document(page: Page, subtitle: &str, body: &str) -> String {
    // The body goes in last, so nothing in it is taken for a placeholder.
    DOCUMENT
        .replace("__TITLE__", page.title())
        .replace("__SITE_CSS__", CSS)
        .replace("__NAV__", &nav(page))
        .replace("__SUBTITLE__", &escape(subtitle))
        .replace("__BODY__", body)
}

/// What the front page shows.
pub struct Home<'a> {
    /// Under the title. Text.
    pub subtitle: &'a str,
    /// The unmapped money with its currency, e.g. `14,325 DKK`. Text.
    pub unmapped: &'a str,
    /// What's left to settle, as HTML made elsewhere.
    pub to_settle: &'a str,
    /// A card for each other page, with a line about it. Text.
    pub cards: &'a [(Page, String)],
}

/// The front page: what needs someone's attention (how much money is
/// unmapped, which leads to the gaps page, and what's left to settle), then
/// a card for each other page.
pub fn home(home: &Home) -> String {
    let cards: String = home
        .cards
        .iter()
        .map(|(page, text)| {
            format!("<a class=\"card\" href=\"{}\"><h2>{}</h2><p>{}</p></a>\n", page.file(), page.title(), escape(text))
        })
        .collect();
    let body = format!(
        "<p class=\"lead\">Unmapped: <a href=\"{}\"><strong>{}</strong></a></p>\n{}<div class=\"cards\">\n{cards}</div>",
        Page::Gaps.file(),
        escape(home.unmapped),
        home.to_settle,
    );
    document(Page::Home, home.subtitle, &body)
}

/// Writes a page into the site's directory.
pub fn write(dir: &Path, page: Page, html: &str) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(page.file());
    std::fs::write(&path, html).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_the_current_page() {
        let nav = nav(Page::Gaps);
        assert!(nav.contains("<a href=\"gaps.html\" aria-current=\"page\">Gaps</a>"), "{nav}");
        assert!(nav.contains("<a href=\"index.html\">Books</a>"), "{nav}");
    }

    #[test]
    fn fills_the_frame() {
        let page = document(Page::Gaps, "a < b", "<p>__NAV__</p>");
        assert!(page.contains("<title>Gaps</title>"));
        assert!(page.contains("a &lt; b"));
        // A placeholder in the body stays as it is.
        assert!(page.contains("<p>__NAV__</p>"));
        assert!(!page.contains("__SITE_CSS__"));
    }
}

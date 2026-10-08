//! Web pages whose audio Tufo mixes, in a browser that runs only while someone uses it.

use std::{fmt, num::NonZeroU32};

use nutype::nutype;
use url::Url;

/// Where words typed into the address field are searched for.
pub const SEARCH_ADDRESS: &str = "https://duckduckgo.com/";
const SEARCH_QUERY_PARAMETER: &str = "q";
const ALLOWED_SCHEMES: [&str; 2] = ["http", "https"];

/// An address a tab may open: only http and https, so typed text never reaches local files or
/// the browser's internal pages.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PageAddress(Url);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PageAddressError {
    #[error("the address is not a URL")]
    Malformed,
    #[error("only http and https addresses open")]
    UnsupportedScheme,
}

impl PageAddress {
    /// Parses a complete http or https URL.
    pub fn parse(text: &str) -> Result<Self, PageAddressError> {
        let url = Url::parse(text).map_err(|_| PageAddressError::Malformed)?;
        if !ALLOWED_SCHEMES.contains(&url.scheme()) {
            return Err(PageAddressError::UnsupportedScheme);
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err(PageAddressError::Malformed);
        }
        Ok(Self(url))
    }

    /// Reads the address field: a URL, a host name to open over https, or else words to search
    /// for. Blank text leads nowhere.
    #[must_use]
    pub fn from_typed(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        if let Ok(address) = Self::parse(text) {
            return Some(address);
        }
        if looks_like_host(text)
            && let Ok(address) = Self::parse(&format!("https://{text}"))
        {
            return Some(address);
        }
        let mut search = Url::parse(SEARCH_ADDRESS).ok()?;
        search
            .query_pairs_mut()
            .append_pair(SEARCH_QUERY_PARAMETER, text);
        Some(Self(search))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for PageAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Text without spaces or a scheme whose host has a dot or names this machine, with an optional
/// numeric port.
fn looks_like_host(text: &str) -> bool {
    if text.chars().any(char::is_whitespace) || text.contains("://") {
        return false;
    }
    let host_and_port = text.split(['/', '?', '#']).next().unwrap_or_default();
    let (host, port) = host_and_port
        .split_once(':')
        .unwrap_or((host_and_port, "0"));
    let port_is_numeric = !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit());
    port_is_numeric && (host.contains('.') || host.eq_ignore_ascii_case("localhost"))
}

/// One tab, for as long as the browser runs.
#[nutype(derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Display))]
pub struct TabId(NonZeroU32);

/// What a tab shows, as the browser reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserTab {
    pub id: TabId,
    /// The address the page committed, which redirects can change from the one requested.
    pub address: String,
    pub title: String,
    pub loading: bool,
    pub history: TabHistory,
    /// The page sends audio now.
    pub audible: bool,
    pub failure: Option<PageFailure>,
}

/// Where a tab can move in its history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TabHistory {
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

/// Why a tab shows no page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageFailure {
    LoadFailed(PageLoadError),
    /// The page's renderer process stopped; reloading starts another.
    Crashed,
}

/// The kinds of load failure the window explains differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageLoadError {
    NameNotResolved,
    /// The connection was refused, reset, closed, or timed out.
    ConnectionFailed,
    Offline,
    InvalidCertificate,
    Blocked,
    Other,
}

/// Every open tab, in order, and the one the view shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrowserTabs {
    pub tabs: Vec<BrowserTab>,
    pub selected: Option<TabId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> Option<String> {
        PageAddress::from_typed(text).map(|address| address.as_str().to_owned())
    }

    #[test]
    fn typed_urls_open_as_written() {
        assert_eq!(
            typed("https://tavern.example/rain?loop=1").as_deref(),
            Some("https://tavern.example/rain?loop=1")
        );
        assert_eq!(
            typed(" http://192.168.0.7:8080/ ").as_deref(),
            Some("http://192.168.0.7:8080/")
        );
    }

    #[test]
    fn typed_host_names_open_over_https() {
        assert_eq!(
            typed("ambience.example/forest").as_deref(),
            Some("https://ambience.example/forest")
        );
        assert_eq!(
            typed("localhost:3000").as_deref(),
            Some("https://localhost:3000/")
        );
        assert_eq!(typed("localhost").as_deref(), Some("https://localhost/"));
    }

    #[test]
    fn typed_words_search() {
        assert_eq!(
            typed("dungeon drips ambience").as_deref(),
            Some("https://duckduckgo.com/?q=dungeon+drips+ambience")
        );
        assert_eq!(
            typed("file:///etc/passwd").as_deref(),
            Some("https://duckduckgo.com/?q=file%3A%2F%2F%2Fetc%2Fpasswd")
        );
        assert_eq!(typed("   "), None);
    }

    #[test]
    fn only_web_addresses_parse() {
        assert!(PageAddress::parse("https://tavern.example/").is_ok());
        assert_eq!(
            PageAddress::parse("file:///home/player/notes.html"),
            Err(PageAddressError::UnsupportedScheme)
        );
        assert_eq!(
            PageAddress::parse("javascript:alert(1)"),
            Err(PageAddressError::UnsupportedScheme)
        );
        assert_eq!(
            PageAddress::parse("chrome://settings"),
            Err(PageAddressError::UnsupportedScheme)
        );
        assert_eq!(
            PageAddress::parse("not a url"),
            Err(PageAddressError::Malformed)
        );
    }
}

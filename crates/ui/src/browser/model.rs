//! Browser state shared by the chrome and platform host. No native handles.
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};

/// Why an ordinary tab refuses a `file:` address, and where local pages open.
pub const LOCAL_FILE_ADDRESS: &str =
    "Local files open from Files: choose Open rendered preview on an HTML file.";

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct PageState {
    pub url: Option<String>,
    pub title: String,
    pub loading: bool,
    pub can_back: bool,
    pub can_forward: bool,
    pub error: Option<String>,
}

impl PageState {
    pub fn label(&self) -> String {
        if !self.title.trim().is_empty() {
            self.title.clone()
        } else {
            self.url
                .as_deref()
                .and_then(|s| url::Url::parse(s).ok())
                .and_then(|u| match u.scheme() {
                    "file" => u
                        .to_file_path()
                        .ok()?
                        .file_name()?
                        .to_str()
                        .map(str::to_owned),
                    _ => u.host_str().map(str::to_owned),
                })
                .unwrap_or_else(|| "Browser".into())
        }
    }
}

pub fn is_html(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
        })
}

/// A workspace HTML file opened with Open rendered preview. Its tab may load
/// and navigate `file:` pages only inside the file's own folder: a generated
/// report reaches its assets and subpages, and nothing else on disk.
/// Ordinary tabs never accept `file:` addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalDocument {
    /// Canonical folder of the document; the page's whole local scope.
    root: PathBuf,
    url: String,
}

impl LocalDocument {
    /// Resolve a Files path inside its local workspace. Both are canonical,
    /// so a symlink cannot move the scope outside the project.
    pub fn in_workspace(workspace: &Path, relative: &str) -> Result<Self, &'static str> {
        let workspace = workspace
            .canonicalize()
            .map_err(|_| "This project folder isn’t available on this device.")?;
        let file = workspace
            .join(relative)
            .canonicalize()
            .ok()
            .filter(|file| file.is_file())
            .ok_or("This file isn’t on disk yet. Save it, then open the rendered preview.")?;
        if !file.starts_with(&workspace) {
            return Err("This file is outside the project, so it can’t be previewed here.");
        }
        if !file.to_str().is_some_and(is_html) {
            return Err("Only HTML files have a rendered preview.");
        }
        let root = file
            .parent()
            .ok_or("This file has no folder to preview from.")?
            .to_path_buf();
        let url = url::Url::from_file_path(&file)
            .map_err(|_| "This file’s location can’t be opened as a page.")?
            .into();
        Ok(Self { root, url })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn folder_name(&self) -> String {
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    /// `address` as WebKit should load it, when it is inside this folder.
    /// The WHATWG form drops a `localhost` authority that GLib would keep.
    pub fn page(&self, address: &str) -> Option<String> {
        if !self.allows(address) {
            return None;
        }
        url::Url::parse(address).ok().map(String::from)
    }

    /// A `file:` address inside this document's folder. Paths are judged
    /// decoded: an escaped separator must not smuggle `..` past URL parsing,
    /// and a symlink inside the folder must not lead out of it.
    pub fn allows(&self, address: &str) -> bool {
        let Ok(url) = url::Url::parse(address) else {
            return false;
        };
        if url.scheme() != "file" || url.host().is_some() {
            return false;
        }
        let Ok(path) = url.to_file_path() else {
            return false;
        };
        path.components().all(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        }) && path.starts_with(&self.root)
            && path != self.root
            && path.canonicalize().map_or(true, |real| {
                real.starts_with(&self.root) && real != self.root
            })
    }
}

pub fn loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host == "localhost" || host.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        None => false,
    }
}

pub fn normalize_address(input: &str) -> Result<String, &'static str> {
    let text = input.trim();
    if text.is_empty() {
        return Err("Enter a website or localhost address.");
    }
    if text.chars().any(|c| c.is_control()) {
        return Err("This address contains invalid characters.");
    }
    // A bare host:port looks like a URI scheme to a URL parser. Only accept
    // that ambiguity when the suffix is an actual numeric port.
    let authority = text.split(['/', '?', '#']).next().unwrap_or(text);
    let host_port = authority
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()));
    let explicit =
        text.contains("://") || (text.contains(':') && !host_port && !text.starts_with('['));
    let mut parsed = url::Url::parse(&if explicit {
        text.to_owned()
    } else {
        format!("https://{text}")
    })
    .map_err(|_| "Enter a valid website or localhost address.")?;
    if parsed.scheme() == "file" {
        return Err(LOCAL_FILE_ADDRESS);
    }
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("Only http and https addresses are supported.");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Use an address without an embedded username or password.");
    }
    if !explicit && loopback(&parsed) {
        let _ = parsed.set_scheme("http");
    }
    Ok(parsed.into())
}

/// Chat links require an explicit web authority; never infer a scheme or
/// silently strip controls, credentials, malformed escapes or backslashes.
pub fn transcript_address(input: &str) -> Result<String, &'static str> {
    let lower = input.to_ascii_lowercase();
    let authority = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .ok_or("Only explicit http and https links are supported.")?;
    if input.chars().any(|c| c.is_control() || c.is_whitespace())
        || input.contains('\\')
        || authority.is_empty()
        || authority.starts_with(['/', '?', '#'])
    {
        return Err("This link contains an invalid address.");
    }
    for (i, byte) in input.bytes().enumerate() {
        if byte == b'%'
            && !input
                .as_bytes()
                .get(i + 1..i + 3)
                .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
        {
            return Err("This link contains an invalid escape.");
        }
    }
    normalize_address(input)
}

pub fn allowed_navigation(address: &str) -> bool {
    url::Url::parse(address).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
    })
}

/// A tab's latest download, shown beneath its toolbar until dismissed. Files
/// are saved to the user's Downloads folder, never displayed as a page.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Download {
    pub name: String,
    /// Set once the whole file is saved.
    #[serde(default)]
    pub path: Option<std::path::PathBuf>,
    /// Why the file was not saved.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Presentation {
    #[default]
    Hidden,
    Live,
    /// Keep rendering while GPUI owns drag input.
    Passthrough,
}

pub fn presentation(active: bool, dragging: bool) -> Presentation {
    if !active {
        Presentation::Hidden
    } else if dragging {
        Presentation::Passthrough
    } else {
        Presentation::Live
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes_web_addresses_and_loopback_ports() {
        for (input, expected) in [
            ("localhost:3000", "http://localhost:3000/"),
            ("127.0.0.1:5173/a?x=1", "http://127.0.0.1:5173/a?x=1"),
            ("[::1]:8080", "http://[::1]:8080/"),
            ("[::1]", "http://[::1]/"),
            (" app.localhost:3000 ", "http://app.localhost:3000/"),
            ("example.com/path", "https://example.com/path"),
            ("https://localhost:3000", "https://localhost:3000/"),
            ("http://example.com", "http://example.com/"),
        ] {
            assert_eq!(normalize_address(input), Ok(expected.into()), "{input}");
        }
    }
    #[test]
    fn rejects_non_web_schemes_credentials_and_bad_input() {
        for input in [
            "",
            "javascript:alert(1)",
            "file:///tmp/a",
            "data:text/html,hi",
            "luna://open/chat/a",
            "https://user:pass@example.com",
            "https://",
            "two words",
            "https://example.com/\nsecret",
        ] {
            assert!(normalize_address(input).is_err(), "{input}");
        }
        assert!(!allowed_navigation("javascript:alert(1)"));
        assert!(!allowed_navigation("https://user@example.com/"));
    }
    #[test]
    fn tab_labels_fall_back_to_host_then_browser() {
        let mut page = PageState::default();
        assert_eq!(page.label(), "Browser");
        page.url = Some("file:///work/report/index.html#?testId=1".into());
        assert_eq!(page.label(), "index.html");
        page.url = Some("http://localhost:3000/path".into());
        assert_eq!(page.label(), "localhost");
        page.title = "Local preview".into();
        assert_eq!(page.label(), "Local preview");
    }

    /// Open rendered preview (#12): a report reaches its own pages and
    /// assets; sibling folders, parents, escapes, other hosts and schemes do
    /// not, and ordinary tabs still refuse every `file:` address.
    #[cfg(unix)]
    #[test]
    fn local_documents_stay_inside_their_folder() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().canonicalize().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let secret = elsewhere.path().canonicalize().unwrap().join("secret.html");
        std::fs::create_dir_all(root.join("report/data")).unwrap();
        std::fs::create_dir_all(root.join("report-old")).unwrap();
        for file in [
            "report/index.html",
            "report/data/attachment.html",
            "report-old/index.html",
            "outside.html",
            "notes.txt",
        ] {
            std::fs::write(root.join(file), "<!doctype html>").unwrap();
        }
        std::fs::write(&secret, "<!doctype html>").unwrap();
        std::os::unix::fs::symlink(&secret, root.join("escape.html")).unwrap();
        std::os::unix::fs::symlink(root.join("report/index.html"), root.join("latest.html"))
            .unwrap();
        // Links inside the report folder: one out of it, one within it.
        std::os::unix::fs::symlink(&secret, root.join("report/escape.html")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), root.join("report/linked")).unwrap();
        std::os::unix::fs::symlink(root.join("report/data"), root.join("report/same")).unwrap();

        let document = LocalDocument::in_workspace(&root, "report/index.html").unwrap();
        let page = |path: &str| {
            format!(
                "{}{path}",
                url::Url::from_directory_path(&root).unwrap().as_str()
            )
        };
        assert_eq!(document.url(), page("report/index.html"));
        assert_eq!(document.folder_name(), "report");
        assert_eq!(
            LocalDocument::in_workspace(&root, "latest.html"),
            Ok(document.clone()),
            "an in-project symlink previews its real file and folder"
        );
        for (address, allowed) in [
            (page("report/index.html#?testId=1"), true),
            (page("report/index.html?q=a"), true),
            (page("report/data/attachment.html"), true),
            (page("report/data/not-yet-written.png"), true),
            (page("report/same/attachment.html"), true),
            (
                format!(
                    "file://localhost{}",
                    root.join("report/index.html").display()
                ),
                true,
            ),
            (page("report/"), false),
            (page("report-old/index.html"), false),
            (page("outside.html"), false),
            (page("report/../outside.html"), false),
            (page("report/%2E%2E/outside.html"), false),
            (page("report/data%2F..%2F..%2Foutside.html"), false),
            (url::Url::from_file_path(&secret).unwrap().into(), false),
            (page("report/escape.html"), false),
            (page("report/linked/secret.html"), false),
            (
                format!(
                    "file://example.com{}",
                    root.join("report/index.html").display()
                ),
                false,
            ),
            ("https://example.com/report/index.html".into(), false),
            ("javascript:alert(1)".into(), false),
        ] {
            assert_eq!(document.allows(&address), allowed, "{address}");
        }
        for relative in [
            "missing.html",
            "report",
            "notes.txt",
            "escape.html",
            secret.to_str().unwrap(),
        ] {
            assert!(
                LocalDocument::in_workspace(&root, relative).is_err(),
                "{relative}"
            );
        }
        assert!(
            LocalDocument::in_workspace(&root.join("report"), "../outside.html").is_err(),
            "a Files path cannot climb out of its project"
        );
        assert_eq!(
            normalize_address(document.url()),
            Err(LOCAL_FILE_ADDRESS),
            "ordinary tabs point local files to Files instead of loading them"
        );
    }
}

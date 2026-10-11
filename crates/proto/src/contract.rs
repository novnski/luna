//! CORE-01 public identifiers and their compatibility rules.
//!
//! Stable namespaced ids name providers, extensions, contributions and content
//! types. They are data, never enum variants. A reader keeps an id it does not
//! recognise byte-for-byte and falls back to a readable default; it never
//! fails, drops or rewrites it. A version or package hash is a separate
//! execution identity and is never part of an id.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

/// Longest accepted id, in bytes.
pub const NAMESPACED_ID_MAX: usize = 128;

/// Namespace for providers that predate namespaced ids.
pub const LEGACY_PROVIDER_NAMESPACE: &str = "luna.provider";

/// Provider wire spellings written before namespaced ids. Each one stays valid
/// forever and maps to `luna.provider/<spelling>`.
pub const LEGACY_PROVIDER_SPELLINGS: &[&str] = &[
    "claude-code",
    "codex",
    "cursor",
    "devin",
    "grok",
    "hermes",
    "pi",
    "opencode",
    "antigravity",
    "mock",
];

/// Why a string is not an acceptable id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError(String);

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for IdError {}

/// `namespace/name`, e.g. `luna.provider/codex`. The namespace is one or more
/// `.`-separated segments matching `[a-z][a-z0-9-]*`; the name matches
/// `[a-z0-9][a-z0-9._-]*`. At most [`NAMESPACED_ID_MAX`] bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct NamespacedId(String);

/// JSON Schema pattern equivalent of [`NamespacedId::parse`].
pub const NAMESPACED_ID_PATTERN: &str =
    r"^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)*/[a-z0-9][a-z0-9._-]*$";

impl NamespacedId {
    pub fn parse(value: &str) -> Result<Self, IdError> {
        if value.len() > NAMESPACED_ID_MAX {
            return Err(IdError(format!(
                "id is longer than {NAMESPACED_ID_MAX} bytes"
            )));
        }
        let Some((namespace, name)) = value.split_once('/') else {
            return Err(IdError(format!(
                "`{value}` has no `/` between namespace and name"
            )));
        };
        let segment = |s: &str| {
            let mut chars = s.chars();
            chars.next().is_some_and(|c| c.is_ascii_lowercase())
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        };
        if !namespace.split('.').all(segment) {
            return Err(IdError(format!("`{value}` has an invalid namespace")));
        }
        let mut chars = name.chars();
        let name_ok = chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && chars.all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
            });
        if !name_ok {
            return Err(IdError(format!("`{value}` has an invalid name")));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn namespace(&self) -> &str {
        self.0
            .split_once('/')
            .map_or("", |(namespace, _)| namespace)
    }

    pub fn name(&self) -> &str {
        self.0.split_once('/').map_or("", |(_, name)| name)
    }
}

impl fmt::Display for NamespacedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for NamespacedId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

macro_rules! namespaced_kind {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub NamespacedId);

        impl $name {
            pub fn parse(value: &str) -> Result<Self, IdError> {
                NamespacedId::parse(value).map(Self)
            }

            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        #[cfg(feature = "schema")]
        impl schemars::JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
                generator.subschema_for::<NamespacedId>()
            }
        }
    };
}

namespaced_kind!(
    /// An installable extension package, e.g. `acme.tools/linter`.
    ExtensionId
);
namespaced_kind!(
    /// One UI or behaviour contribution an extension declares.
    ContributionId
);
namespaced_kind!(
    /// A durable content type a renderer registers for.
    ContentTypeId
);

/// A provider reference: a legacy wire spelling (`codex`, `claude-code`, ...)
/// or a namespaced id. Kept byte-for-byte as received, so a payload written
/// with a legacy spelling serialises, and hashes, exactly as before.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ProviderRef(String);

impl ProviderRef {
    pub fn parse(value: &str) -> Result<Self, IdError> {
        if LEGACY_PROVIDER_SPELLINGS.contains(&value) {
            return Ok(Self(value.to_owned()));
        }
        NamespacedId::parse(value)
            .map(|_| Self(value.to_owned()))
            .map_err(|error| IdError(format!("not a legacy provider spelling or {error}")))
    }

    /// The spelling exactly as received.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_legacy(&self) -> bool {
        LEGACY_PROVIDER_SPELLINGS.contains(&self.0.as_str())
    }

    /// The canonical namespaced id; legacy spellings map to `luna.provider/<spelling>`.
    pub fn canonical(&self) -> NamespacedId {
        if self.is_legacy() {
            NamespacedId(format!("{LEGACY_PROVIDER_NAMESPACE}/{}", self.0))
        } else {
            NamespacedId(self.0.clone())
        }
    }
}

impl From<crate::HarnessId> for ProviderRef {
    fn from(id: crate::HarnessId) -> Self {
        let spelling = serde_json::to_value(id)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        Self(spelling)
    }
}

impl<'de> Deserialize<'de> for ProviderRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for NamespacedId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "NamespacedId".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": NAMESPACED_ID_PATTERN,
            "maxLength": NAMESPACED_ID_MAX,
            "description": "namespace/name; unknown ids are preserved verbatim by readers",
        })
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for ProviderRef {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ProviderRef".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let namespaced = generator.subschema_for::<NamespacedId>();
        schemars::json_schema!({
            "description": "A legacy provider spelling (kept byte-for-byte) or a namespaced provider id",
            "anyOf": [
                { "type": "string", "enum": LEGACY_PROVIDER_SPELLINGS },
                namespaced,
            ],
        })
    }
}

//! Stable provider identifiers. The bounded wire ID is a value: no interning
//! table, allocation lifetime, or compiled variant is needed for a new provider.
use crate::contract::{
    IdError, LEGACY_PROVIDER_NAMESPACE, LEGACY_PROVIDER_SPELLINGS, NAMESPACED_ID_MAX, ProviderRef,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
};

/// Compatibility classification for the built-in implementations only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinHarnessId {
    ClaudeCode,
    Codex,
    Cursor,
    Devin,
    Grok,
    Hermes,
    Pi,
    Opencode,
    Antigravity,
    Mock,
    Other,
}

/// A validated provider reference with lossless wire spelling. Kept inline
/// under CORE-01's existing 128-byte limit so identity remains a bounded Copy
/// value in the engine's immutable task bindings and client projections.
#[derive(Clone, Copy)]
pub struct HarnessId {
    bytes: [u8; NAMESPACED_ID_MAX],
    len: u8,
}

#[allow(non_upper_case_globals)]
impl HarnessId {
    pub const ClaudeCode: Self = Self::literal("claude-code");
    pub const Codex: Self = Self::literal("codex");
    pub const Cursor: Self = Self::literal("cursor");
    pub const Devin: Self = Self::literal("devin");
    pub const Grok: Self = Self::literal("grok");
    pub const Hermes: Self = Self::literal("hermes");
    pub const Pi: Self = Self::literal("pi");
    pub const Opencode: Self = Self::literal("opencode");
    pub const Antigravity: Self = Self::literal("antigravity");
    pub const Mock: Self = Self::literal("mock");

    const fn literal(value: &str) -> Self {
        let mut bytes = [0; NAMESPACED_ID_MAX];
        let mut index = 0;
        while index < value.len() {
            bytes[index] = value.as_bytes()[index];
            index += 1;
        }
        Self {
            bytes,
            len: value.len() as u8,
        }
    }
    pub fn parse(value: &str) -> Result<Self, IdError> {
        ProviderRef::parse(value)?;
        Ok(Self::literal(value))
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len as usize]).expect("validated ASCII provider ID")
    }
    fn key(&self) -> (&str, &str) {
        let value = self.as_str();
        if LEGACY_PROVIDER_SPELLINGS.contains(&value) {
            (LEGACY_PROVIDER_NAMESPACE, value)
        } else {
            value.split_once('/').expect("validated provider namespace")
        }
    }
    pub fn canonical(&self) -> ProviderRef {
        let (namespace, name) = self.key();
        ProviderRef::parse(&format!("{namespace}/{name}")).expect("validated provider ID")
    }
    pub fn kind(self) -> BuiltinHarnessId {
        let (namespace, name) = self.key();
        if namespace != LEGACY_PROVIDER_NAMESPACE {
            return BuiltinHarnessId::Other;
        }
        match name {
            "claude-code" => BuiltinHarnessId::ClaudeCode,
            "codex" => BuiltinHarnessId::Codex,
            "cursor" => BuiltinHarnessId::Cursor,
            "devin" => BuiltinHarnessId::Devin,
            "grok" => BuiltinHarnessId::Grok,
            "hermes" => BuiltinHarnessId::Hermes,
            "pi" => BuiltinHarnessId::Pi,
            "opencode" => BuiltinHarnessId::Opencode,
            "antigravity" => BuiltinHarnessId::Antigravity,
            "mock" => BuiltinHarnessId::Mock,
            _ => BuiltinHarnessId::Other,
        }
    }
    pub fn is_retired(self) -> bool {
        matches!(
            self.kind(),
            BuiltinHarnessId::Cursor
                | BuiltinHarnessId::Devin
                | BuiltinHarnessId::Grok
                | BuiltinHarnessId::Hermes
                | BuiltinHarnessId::Pi
                | BuiltinHarnessId::Antigravity
        )
    }
}
impl PartialEq for HarnessId {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}
impl Eq for HarnessId {}
impl Hash for HarnessId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}
impl PartialOrd for HarnessId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HarnessId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}
impl fmt::Debug for HarnessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl fmt::Display for HarnessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl Serialize for HarnessId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for HarnessId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

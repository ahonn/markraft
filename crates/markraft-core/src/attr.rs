//! Attribute values and ordered attribute maps.
//!
//! Attributes are the per-node and per-mark parameters declared by a schema.
//! They are stored as a small ordered map so that two structurally equal nodes
//! always compare and serialise identically.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A JSON-compatible attribute value.
///
/// Floats participate in `Eq`/`Ord` through their bit pattern, which keeps the
/// document model totally ordered without permitting `NaN` surprises in
/// canonical mark ordering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttrValue {
    /// JSON `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// A 64-bit signed integer.
    Int(i64),
    /// A double-precision float.
    Float(f64),
    /// A UTF-8 string.
    Str(String),
}

impl AttrValue {
    /// Name of the value kind, used in error messages and [`AttrKind`] checks.
    pub fn kind(&self) -> AttrKind {
        match self {
            AttrValue::Null => AttrKind::Null,
            AttrValue::Bool(_) => AttrKind::Bool,
            AttrValue::Int(_) => AttrKind::Int,
            AttrValue::Float(_) => AttrKind::Float,
            AttrValue::Str(_) => AttrKind::Str,
        }
    }

    /// The string contents, if this is a [`AttrValue::Str`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            AttrValue::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The integer contents, if this is a [`AttrValue::Int`].
    pub fn as_int(&self) -> Option<i64> {
        match self {
            AttrValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// The boolean contents, if this is a [`AttrValue::Bool`].
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            AttrValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    fn order_key(&self) -> (u8, i64, u64, &str) {
        match self {
            AttrValue::Null => (0, 0, 0, ""),
            AttrValue::Bool(b) => (1, *b as i64, 0, ""),
            AttrValue::Int(i) => (2, *i, 0, ""),
            AttrValue::Float(f) => (3, 0, f.to_bits(), ""),
            AttrValue::Str(s) => (4, 0, 0, s.as_str()),
        }
    }
}

impl PartialEq for AttrValue {
    fn eq(&self, other: &Self) -> bool {
        self.order_key() == other.order_key()
    }
}

impl Eq for AttrValue {}

impl Hash for AttrValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.order_key().hash(state);
    }
}

impl PartialOrd for AttrValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AttrValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order_key().cmp(&other.order_key())
    }
}

impl From<bool> for AttrValue {
    fn from(v: bool) -> Self {
        AttrValue::Bool(v)
    }
}

impl From<i64> for AttrValue {
    fn from(v: i64) -> Self {
        AttrValue::Int(v)
    }
}

impl From<f64> for AttrValue {
    fn from(v: f64) -> Self {
        AttrValue::Float(v)
    }
}

impl From<&str> for AttrValue {
    fn from(v: &str) -> Self {
        AttrValue::Str(v.to_string())
    }
}

impl From<String> for AttrValue {
    fn from(v: String) -> Self {
        AttrValue::Str(v)
    }
}

/// The kind of value an attribute accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AttrKind {
    /// Only `null`.
    Null,
    /// Booleans.
    Bool,
    /// Integers.
    Int,
    /// Floats. Also accepts [`AttrValue::Int`], which is widened on validation.
    Float,
    /// Strings.
    Str,
    /// Any of the above.
    Any,
}

impl AttrKind {
    /// Whether `value` satisfies this kind.
    pub fn accepts(self, value: &AttrValue) -> bool {
        match self {
            AttrKind::Any => true,
            AttrKind::Float => matches!(value, AttrValue::Float(_) | AttrValue::Int(_)),
            other => other == value.kind(),
        }
    }
}

impl fmt::Display for AttrKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            AttrKind::Null => "null",
            AttrKind::Bool => "bool",
            AttrKind::Int => "int",
            AttrKind::Float => "float",
            AttrKind::Str => "string",
            AttrKind::Any => "any",
        };
        f.write_str(name)
    }
}

/// Declaration of a single attribute on a node or mark type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrSpec {
    /// Attribute name.
    pub name: String,
    /// Accepted value kind.
    pub kind: AttrKind,
    /// Value used when the attribute is not given. `None` makes the attribute
    /// required, which excludes the type from automatic node creation
    /// (`fill_before`, `find_wrapping`, default types).
    pub default: Option<AttrValue>,
}

impl AttrSpec {
    /// An optional attribute with a default value.
    pub fn new(name: impl Into<String>, kind: AttrKind, default: AttrValue) -> Self {
        AttrSpec {
            name: name.into(),
            kind,
            default: Some(default),
        }
    }

    /// An attribute that must be supplied explicitly.
    pub fn required(name: impl Into<String>, kind: AttrKind) -> Self {
        AttrSpec {
            name: name.into(),
            kind,
            default: None,
        }
    }
}

/// An ordered attribute map, sorted by name.
///
/// Cloning is a reference-count bump. The empty map does not allocate.
#[derive(Debug, Clone, Default)]
pub struct Attrs(Option<Arc<Vec<(String, AttrValue)>>>);

impl Attrs {
    /// The empty attribute map.
    pub fn empty() -> Attrs {
        Attrs(None)
    }

    /// Build a map from unordered pairs. Later entries win over earlier ones
    /// with the same name.
    pub fn from_pairs<I, K, V>(pairs: I) -> Attrs
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<AttrValue>,
    {
        let mut entries: Vec<(String, AttrValue)> = Vec::new();
        for (k, v) in pairs {
            let k = k.into();
            match entries.binary_search_by(|(name, _)| name.as_str().cmp(k.as_str())) {
                Ok(i) => entries[i].1 = v.into(),
                Err(i) => entries.insert(i, (k, v.into())),
            }
        }
        if entries.is_empty() {
            Attrs(None)
        } else {
            Attrs(Some(Arc::new(entries)))
        }
    }

    /// Whether the map holds no entries.
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |v| v.len())
    }

    /// Look up an attribute by name.
    pub fn get(&self, name: &str) -> Option<&AttrValue> {
        let entries = self.0.as_ref()?;
        entries
            .binary_search_by(|(n, _)| n.as_str().cmp(name))
            .ok()
            .map(|i| &entries[i].1)
    }

    /// Iterate over the entries in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &AttrValue)> {
        self.0
            .iter()
            .flat_map(|v| v.iter().map(|(k, val)| (k.as_str(), val)))
    }

    /// Return a copy with `name` set to `value`.
    pub fn with(&self, name: impl Into<String>, value: impl Into<AttrValue>) -> Attrs {
        let mut entries: Vec<(String, AttrValue)> =
            self.0.as_ref().map(|v| (**v).clone()).unwrap_or_default();
        let name = name.into();
        match entries.binary_search_by(|(n, _)| n.as_str().cmp(name.as_str())) {
            Ok(i) => entries[i].1 = value.into(),
            Err(i) => entries.insert(i, (name, value.into())),
        }
        Attrs(Some(Arc::new(entries)))
    }

    /// Whether both maps are the very same allocation, not only equal.
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) fn shares(&self, other: &Attrs) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    fn slice(&self) -> &[(String, AttrValue)] {
        self.0.as_ref().map_or(&[], |v| v.as_slice())
    }
}

impl PartialEq for Attrs {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
            (None, None) => true,
            _ => false,
        }
    }
}

impl Eq for Attrs {}

impl Hash for Attrs {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.slice().hash(state);
    }
}

impl PartialOrd for Attrs {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Attrs {
    fn cmp(&self, other: &Self) -> Ordering {
        self.slice().cmp(other.slice())
    }
}

/// Convenience macro for building [`Attrs`].
///
/// ```
/// # use markraft_core::{attrs, AttrValue};
/// let a = attrs!{"level" => 2i64, "id" => "intro"};
/// assert_eq!(a.get("level"), Some(&AttrValue::Int(2)));
/// ```
#[macro_export]
macro_rules! attrs {
    () => { $crate::Attrs::empty() };
    ($($k:expr => $v:expr),+ $(,)?) => {
        $crate::Attrs::from_pairs([$(($k, $crate::AttrValue::from($v))),+])
    };
}

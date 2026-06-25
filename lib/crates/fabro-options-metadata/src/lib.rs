use std::collections::BTreeMap;
use std::fmt::{Debug, Display, Formatter};

use serde::{Serialize, Serializer};

/// Visits [`OptionsMetadata`] entries.
pub trait Visit {
    /// Record a single option field.
    fn record_field(&mut self, name: &str, field: OptionField);

    /// Record a nested option set.
    fn record_set(&mut self, name: &str, set: OptionSet);
}

/// Returns metadata for a type's options.
pub trait OptionsMetadata {
    /// Visits each option in this type.
    fn record(visit: &mut dyn Visit);

    /// Returns documentation for the whole option set.
    fn documentation() -> Option<&'static str> {
        None
    }

    /// Returns the extracted metadata set.
    fn metadata() -> OptionSet
    where
        Self: Sized + 'static,
    {
        OptionSet::of::<Self>()
    }
}

impl<T> OptionsMetadata for Option<T>
where
    T: OptionsMetadata,
{
    fn record(visit: &mut dyn Visit) {
        T::record(visit);
    }
}

/// Metadata for an option entry, either a field or a nested option set.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(untagged)]
pub enum OptionEntry {
    /// A single option.
    Field(OptionField),
    /// A nested set of options.
    Set(OptionSet),
}

impl Display for OptionEntry {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Field(field) => Display::fmt(field, formatter),
            Self::Set(set) => Display::fmt(set, formatter),
        }
    }
}

/// A set of options for a type implementing [`OptionsMetadata`].
#[derive(Copy, Clone)]
pub struct OptionSet {
    record: fn(&mut dyn Visit),
    doc: fn() -> Option<&'static str>,
}

impl OptionSet {
    /// Create an option set for a type.
    pub fn of<T>() -> Self
    where
        T: OptionsMetadata + 'static,
    {
        Self {
            record: T::record,
            doc: T::documentation,
        }
    }

    /// Visit each option in this set.
    pub fn record(&self, visit: &mut dyn Visit) {
        (self.record)(visit);
    }

    /// Returns documentation for this option set.
    pub fn documentation(&self) -> Option<&'static str> {
        (self.doc)()
    }

    /// Returns true if this set contains an option by dotted name.
    pub fn has(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    /// Find an option by dotted name.
    pub fn find(&self, name: &str) -> Option<OptionEntry> {
        struct FindVisitor<'a> {
            entry: Option<OptionEntry>,
            needle: &'a str,
            parts: std::str::Split<'a, char>,
        }

        impl Visit for FindVisitor<'_> {
            fn record_field(&mut self, name: &str, field: OptionField) {
                if self.entry.is_none() && name == self.needle && self.parts.next().is_none() {
                    self.entry = Some(OptionEntry::Field(field));
                }
            }

            fn record_set(&mut self, name: &str, set: OptionSet) {
                if self.entry.is_none() && name == self.needle {
                    if let Some(next) = self.parts.next() {
                        self.needle = next;
                        set.record(self);
                    } else {
                        self.entry = Some(OptionEntry::Set(set));
                    }
                }
            }
        }

        let mut parts = name.split('.');
        let first = parts.next()?;
        let mut visitor = FindVisitor {
            entry: None,
            needle: first,
            parts,
        };
        self.record(&mut visitor);
        visitor.entry
    }

    /// Returns all field entries flattened by dotted name.
    pub fn fields(&self) -> BTreeMap<String, OptionField> {
        struct FieldsVisitor<'a> {
            entries: &'a mut BTreeMap<String, OptionField>,
            prefix: String,
        }

        impl Visit for FieldsVisitor<'_> {
            fn record_field(&mut self, name: &str, field: OptionField) {
                self.entries
                    .insert(format!("{}{}", self.prefix, name), field);
            }

            fn record_set(&mut self, name: &str, set: OptionSet) {
                let previous = self.prefix.clone();
                self.prefix.push_str(name);
                self.prefix.push('.');
                set.record(self);
                self.prefix = previous;
            }
        }

        let mut entries = BTreeMap::new();
        self.record(&mut FieldsVisitor {
            entries: &mut entries,
            prefix: String::new(),
        });
        entries
    }
}

impl PartialEq for OptionSet {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::fn_addr_eq(self.record, other.record) && std::ptr::fn_addr_eq(self.doc, other.doc)
    }
}

impl Eq for OptionSet {}

impl Display for OptionSet {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        struct DisplayVisitor<'a, 'b> {
            formatter: &'a mut Formatter<'b>,
            result: std::fmt::Result,
        }

        impl Visit for DisplayVisitor<'_, '_> {
            fn record_field(&mut self, name: &str, field: OptionField) {
                self.result = self.result.and_then(|()| {
                    write!(self.formatter, "{name}")?;
                    if field.deprecated.is_some() {
                        write!(self.formatter, " (deprecated)")?;
                    }
                    writeln!(self.formatter)
                });
            }

            fn record_set(&mut self, name: &str, _set: OptionSet) {
                self.result = self
                    .result
                    .and_then(|()| writeln!(self.formatter, "{name}"));
            }
        }

        let mut visitor = DisplayVisitor {
            formatter,
            result: Ok(()),
        };
        self.record(&mut visitor);
        visitor.result
    }
}

impl Debug for OptionSet {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self, formatter)
    }
}

impl Serialize for OptionSet {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.fields().serialize(serializer)
    }
}

/// Metadata for a single option field.
#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
pub struct OptionField {
    /// Option documentation from doc comments, when present.
    pub doc: Option<&'static str>,
    /// The option's default value, formatted for docs.
    pub default: Option<&'static str>,
    /// The option value type, formatted for docs.
    pub value_type: Option<&'static str>,
    /// Optional scope, for docs that group settings by source.
    pub scope: Option<&'static str>,
    /// Example usage for the option.
    pub example: Option<&'static str>,
    /// Deprecation metadata.
    pub deprecated: Option<Deprecated>,
    /// Possible values for enum-like options.
    pub possible_values: Option<Vec<PossibleValue>>,
    /// Version where this option was added.
    pub added_in: Option<&'static str>,
}

impl Display for OptionField {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(doc) = self.doc {
            writeln!(formatter, "{doc}")?;
            writeln!(formatter)?;
        }

        if let Some(default) = self.default {
            writeln!(formatter, "Default value: {default}")?;
        }

        if let Some(possible_values) = self
            .possible_values
            .as_ref()
            .filter(|values| !values.is_empty())
        {
            writeln!(formatter, "Possible values:")?;
            for value in possible_values {
                writeln!(formatter, "- {value}")?;
            }
        } else if let Some(value_type) = self.value_type {
            writeln!(formatter, "Type: {value_type}")?;
        }

        if let Some(deprecated) = &self.deprecated {
            write!(formatter, "Deprecated")?;
            if let Some(since) = deprecated.since {
                write!(formatter, " (since {since})")?;
            }
            if let Some(message) = deprecated.message {
                write!(formatter, ": {message}")?;
            }
            writeln!(formatter)?;
        }

        if let Some(example) = self.example {
            writeln!(formatter, "Example usage:\n```toml\n{example}\n```")?;
        }

        Ok(())
    }
}

/// Deprecation metadata for an option.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct Deprecated {
    /// Version where the option was deprecated.
    pub since: Option<&'static str>,
    /// Deprecation message.
    pub message: Option<&'static str>,
}

/// A possible value for an enum-like option.
#[derive(Debug, Eq, PartialEq, Clone, Serialize)]
pub struct PossibleValue {
    /// Value name as it appears on the CLI or in config.
    pub name: String,
    /// Optional value help text.
    pub help: Option<String>,
}

impl Display for PossibleValue {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "`\"{}\"`", self.name)?;
        if let Some(help) = &self.help {
            write!(formatter, ": {help}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(doc: Option<&'static str>) -> OptionField {
        OptionField {
            doc,
            default: None,
            value_type: Some("bool"),
            scope: None,
            example: None,
            deprecated: None,
            possible_values: None,
            added_in: None,
        }
    }

    #[test]
    fn option_set_finds_child_and_nested_options() {
        struct Root;
        struct Nested;

        impl OptionsMetadata for Root {
            fn record(visit: &mut dyn Visit) {
                visit.record_field("verbose", field(Some("Enable verbose output.")));
                visit.record_set("nested", Nested::metadata());
            }
        }

        impl OptionsMetadata for Nested {
            fn record(visit: &mut dyn Visit) {
                visit.record_field("dry-run", field(Some("Preview the work.")));
            }
        }

        assert!(Root::metadata().has("verbose"));
        assert!(Root::metadata().has("nested.dry-run"));
        assert!(!Root::metadata().has("nested.missing"));
        assert!(matches!(
            Root::metadata().find("nested"),
            Some(OptionEntry::Set(_))
        ));
        assert_eq!(
            Root::metadata().find("nested.dry-run"),
            Some(OptionEntry::Field(field(Some("Preview the work."))))
        );
    }

    #[test]
    fn option_set_display_lists_fields_and_sets() {
        struct Root;
        struct Nested;

        impl OptionsMetadata for Root {
            fn record(visit: &mut dyn Visit) {
                visit.record_field("verbose", field(Some("Enable verbose output.")));
                visit.record_set("nested", Nested::metadata());
            }
        }

        impl OptionsMetadata for Nested {
            fn record(_visit: &mut dyn Visit) {}
        }

        assert_eq!(Root::metadata().to_string(), "verbose\nnested\n");
    }

    #[test]
    fn option_set_serializes_nested_fields_with_dot_keys() {
        struct Root;
        struct Nested;

        impl OptionsMetadata for Root {
            fn record(visit: &mut dyn Visit) {
                visit.record_set("nested", Nested::metadata());
            }
        }

        impl OptionsMetadata for Nested {
            fn record(visit: &mut dyn Visit) {
                visit.record_field("dry-run", field(Some("Preview the work.")));
            }
        }

        let json = serde_json::to_value(Root::metadata()).expect("metadata should serialize");
        assert_eq!(
            json["nested.dry-run"]["doc"],
            serde_json::json!("Preview the work.")
        );
    }

    #[test]
    fn option_set_fields_flatten_nested_fields_with_dot_keys() {
        struct Root;
        struct Nested;

        impl OptionsMetadata for Root {
            fn record(visit: &mut dyn Visit) {
                visit.record_field("verbose", field(Some("Enable verbose output.")));
                visit.record_set("nested", Nested::metadata());
            }
        }

        impl OptionsMetadata for Nested {
            fn record(visit: &mut dyn Visit) {
                visit.record_field("dry-run", field(Some("Preview the work.")));
            }
        }

        let fields = Root::metadata().fields();
        assert_eq!(fields.len(), 2);
        assert_eq!(
            fields.get("nested.dry-run"),
            Some(&field(Some("Preview the work.")))
        );
        assert_eq!(
            fields.get("verbose"),
            Some(&field(Some("Enable verbose output.")))
        );
    }

    #[test]
    fn field_doc_can_be_absent() {
        struct Root;

        impl OptionsMetadata for Root {
            fn record(visit: &mut dyn Visit) {
                visit.record_field("undocumented", field(None));
            }
        }

        assert_eq!(
            Root::metadata().find("undocumented"),
            Some(OptionEntry::Field(field(None)))
        );
    }
}

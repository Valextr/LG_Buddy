use std::collections::HashSet;
use std::fmt;

use crate::config::DEFAULT_IDLE_TIMEOUT;

const READ_WRITE_OPERATIONS: &[SettingOperation] = &[
    SettingOperation::Get,
    SettingOperation::Describe,
    SettingOperation::Set,
    SettingOperation::Unset,
];
const READ_ONLY_OPERATIONS: &[SettingOperation] =
    &[SettingOperation::Get, SettingOperation::Describe];

const EMPTY_ALIASES: &[SettingAlias] = &[];
const SCREEN_RESTORE_POLICY_ALIASES: &[SettingAlias] = &[SettingAlias {
    from: "marker_only",
    to: "conservative",
}];

const SCREEN_BACKEND_VALUES: &[&str] = &["auto", "gnome", "swayidle"];
const SCREEN_RESTORE_POLICY_VALUES: &[&str] = &["conservative", "aggressive"];
const SYSTEM_SLEEP_WAKE_POLICY_VALUES: &[&str] = &["enabled", "disabled"];

const SETTING_DEFINITIONS: &[SettingDefinition] = &[
    SettingDefinition {
        key: "screen.backend",
        storage_key: "screen_backend",
        value_type: SettingType::Enum(EnumSettingType {
            values: SCREEN_BACKEND_VALUES,
            aliases: EMPTY_ALIASES,
        }),
        default_value: SettingValue::Enum("auto"),
        mutability: SettingMutability::ReadWrite,
        operations: READ_WRITE_OPERATIONS,
        apply_strategy: ApplyStrategy::RestartUserScreenService,
        description: "Screen backend selection for user-session blanking and restore behavior.",
    },
    SettingDefinition {
        key: "screen.idle_timeout",
        storage_key: "screen_idle_timeout",
        value_type: SettingType::Integer(IntegerSettingType {
            min: 1,
            max: 86_400,
        }),
        default_value: SettingValue::Integer(DEFAULT_IDLE_TIMEOUT as i64),
        mutability: SettingMutability::ReadWrite,
        operations: READ_WRITE_OPERATIONS,
        apply_strategy: ApplyStrategy::RestartUserScreenService,
        description: "Idle timeout in seconds before LG Buddy blanks the configured screen.",
    },
    SettingDefinition {
        key: "screen.restore_policy",
        storage_key: "screen_restore_policy",
        value_type: SettingType::Enum(EnumSettingType {
            values: SCREEN_RESTORE_POLICY_VALUES,
            aliases: SCREEN_RESTORE_POLICY_ALIASES,
        }),
        default_value: SettingValue::Enum("conservative"),
        mutability: SettingMutability::ReadWrite,
        operations: READ_WRITE_OPERATIONS,
        apply_strategy: ApplyStrategy::RestartUserScreenService,
        description: "Screen restore policy after LG Buddy blanks the configured screen.",
    },
    SettingDefinition {
        key: "system.sleep_wake_policy",
        storage_key: "system_sleep_wake_policy",
        value_type: SettingType::Enum(EnumSettingType {
            values: SYSTEM_SLEEP_WAKE_POLICY_VALUES,
            aliases: EMPTY_ALIASES,
        }),
        default_value: SettingValue::Enum("enabled"),
        mutability: SettingMutability::ReadOnly,
        operations: READ_ONLY_OPERATIONS,
        apply_strategy: ApplyStrategy::PendingLifecycleService,
        description: "System sleep and wake policy for lifecycle hooks.",
    },
];

pub static SETTINGS_REGISTRY: SettingsRegistry = SettingsRegistry {
    definitions: SETTING_DEFINITIONS,
};

#[derive(Debug, Clone, Copy)]
pub struct SettingsRegistry {
    definitions: &'static [SettingDefinition],
}

impl SettingsRegistry {
    pub fn all(&self) -> &'static [SettingDefinition] {
        self.definitions
    }

    pub fn get(&self, key: SettingKey<'_>) -> Result<&'static SettingDefinition, SettingsError> {
        self.definitions
            .iter()
            .find(|definition| definition.key == key.as_str())
            .ok_or_else(|| SettingsError::UnknownKey(key.as_str().to_string()))
    }

    pub fn get_by_name(&self, key: &str) -> Result<&'static SettingDefinition, SettingsError> {
        self.get(SettingKey::parse(key)?)
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        let mut public_keys = HashSet::new();
        let mut storage_keys = HashSet::new();

        for definition in self.definitions {
            SettingKey::parse(definition.key)?;
            validate_storage_key(definition.storage_key).map_err(|reason| {
                SettingsError::RegistryInvariant(format!(
                    "invalid storage key `{}` for `{}`: {reason}",
                    definition.storage_key, definition.key
                ))
            })?;

            if !public_keys.insert(definition.key) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "duplicate setting key `{}`",
                    definition.key
                )));
            }

            if !storage_keys.insert(definition.storage_key) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "duplicate storage key `{}`",
                    definition.storage_key
                )));
            }

            definition.validate_type_metadata()?;
            definition.validate_default()?;
            definition.validate_operation_metadata()?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SettingKey<'a>(&'a str);

impl<'a> SettingKey<'a> {
    pub fn parse(value: &'a str) -> Result<Self, SettingsError> {
        validate_setting_key(value)
            .map_err(|reason| SettingsError::InvalidKey {
                key: value.to_string(),
                reason,
            })
            .map(|()| Self(value))
    }

    pub fn as_str(self) -> &'a str {
        self.0
    }
}

impl fmt::Display for SettingKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SettingDefinition {
    key: &'static str,
    storage_key: &'static str,
    value_type: SettingType,
    default_value: SettingValue,
    mutability: SettingMutability,
    operations: &'static [SettingOperation],
    apply_strategy: ApplyStrategy,
    description: &'static str,
}

impl SettingDefinition {
    pub fn key(&self) -> SettingKey<'static> {
        SettingKey(self.key)
    }

    pub fn key_name(&self) -> &'static str {
        self.key
    }

    pub fn storage_key(&self) -> &'static str {
        self.storage_key
    }

    pub fn value_type(&self) -> SettingType {
        self.value_type
    }

    pub fn default_value(&self) -> SettingValue {
        self.default_value
    }

    pub fn mutability(&self) -> SettingMutability {
        self.mutability
    }

    pub fn supported_operations(&self) -> &'static [SettingOperation] {
        self.operations
    }

    pub fn apply_strategy(&self) -> ApplyStrategy {
        self.apply_strategy
    }

    pub fn description(&self) -> &'static str {
        self.description
    }

    pub fn supports_operation(&self, operation: SettingOperation) -> bool {
        self.operations.contains(&operation)
    }

    pub fn ensure_operation_supported(
        &self,
        operation: SettingOperation,
    ) -> Result<(), SettingsError> {
        if self.supports_operation(operation) {
            Ok(())
        } else {
            Err(SettingsError::UnsupportedOperation {
                key: self.key.to_string(),
                operation,
            })
        }
    }

    pub fn parse_value(&self, value: &str) -> Result<SettingValue, SettingsError> {
        self.value_type.parse_value(self.key, value)
    }

    fn validate_type_metadata(&self) -> Result<(), SettingsError> {
        match self.value_type {
            SettingType::Enum(enum_type) => enum_type.validate(self.key),
            SettingType::Integer(integer_type) => integer_type.validate(self.key),
        }
    }

    fn validate_default(&self) -> Result<(), SettingsError> {
        self.value_type
            .validate_value(self.key, self.default_value)
            .map_err(|err| match err {
                SettingsError::InvalidValue {
                    key,
                    value,
                    expected,
                } => SettingsError::RegistryInvariant(format!(
                    "invalid default value `{value}` for `{key}`: expected {expected}"
                )),
                other => other,
            })
    }

    fn validate_operation_metadata(&self) -> Result<(), SettingsError> {
        if self.operations.is_empty() {
            return Err(SettingsError::RegistryInvariant(format!(
                "`{}` must support at least one operation",
                self.key
            )));
        }

        match self.mutability {
            SettingMutability::ReadWrite => {
                for operation in READ_WRITE_OPERATIONS {
                    if !self.operations.contains(operation) {
                        return Err(SettingsError::RegistryInvariant(format!(
                            "`{}` is read-write but does not support `{}`",
                            self.key,
                            operation.as_str()
                        )));
                    }
                }
            }
            SettingMutability::ReadOnly => {
                for operation in [SettingOperation::Set, SettingOperation::Unset] {
                    if self.operations.contains(&operation) {
                        return Err(SettingsError::RegistryInvariant(format!(
                            "`{}` is read-only but supports `{}`",
                            self.key,
                            operation.as_str()
                        )));
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingType {
    Enum(EnumSettingType),
    Integer(IntegerSettingType),
}

impl SettingType {
    pub fn parse_value(self, key: &str, value: &str) -> Result<SettingValue, SettingsError> {
        match self {
            Self::Enum(enum_type) => enum_type
                .canonicalize(value)
                .map(SettingValue::Enum)
                .ok_or_else(|| SettingsError::InvalidValue {
                    key: key.to_string(),
                    value: value.to_string(),
                    expected: enum_type.expected(),
                }),
            Self::Integer(integer_type) => match value.parse::<i64>() {
                Ok(parsed) if integer_type.contains(parsed) => Ok(SettingValue::Integer(parsed)),
                _ => Err(SettingsError::InvalidValue {
                    key: key.to_string(),
                    value: value.to_string(),
                    expected: integer_type.expected(),
                }),
            },
        }
    }

    pub fn validate_value(self, key: &str, value: SettingValue) -> Result<(), SettingsError> {
        match (self, value) {
            (Self::Enum(enum_type), SettingValue::Enum(value)) => {
                match enum_type.canonicalize(value) {
                    Some(canonical) if canonical == value => Ok(()),
                    _ => Err(SettingsError::InvalidValue {
                        key: key.to_string(),
                        value: value.to_string(),
                        expected: enum_type.expected(),
                    }),
                }
            }
            (Self::Integer(integer_type), SettingValue::Integer(value))
                if integer_type.contains(value) =>
            {
                Ok(())
            }
            (Self::Integer(integer_type), SettingValue::Integer(value)) => {
                Err(SettingsError::InvalidValue {
                    key: key.to_string(),
                    value: value.to_string(),
                    expected: integer_type.expected(),
                })
            }
            (expected_type, actual_value) => Err(SettingsError::InvalidValue {
                key: key.to_string(),
                value: actual_value.to_string(),
                expected: expected_type.expected(),
            }),
        }
    }

    pub fn expected(self) -> String {
        match self {
            Self::Enum(enum_type) => enum_type.expected(),
            Self::Integer(integer_type) => integer_type.expected(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumSettingType {
    values: &'static [&'static str],
    aliases: &'static [SettingAlias],
}

impl EnumSettingType {
    pub fn values(self) -> &'static [&'static str] {
        self.values
    }

    pub fn aliases(self) -> &'static [SettingAlias] {
        self.aliases
    }

    pub fn canonicalize(self, value: &str) -> Option<&'static str> {
        if let Some(canonical) = self
            .values
            .iter()
            .copied()
            .find(|allowed| *allowed == value)
        {
            return Some(canonical);
        }

        self.aliases
            .iter()
            .find(|alias| alias.from == value)
            .map(|alias| alias.to)
    }

    fn expected(self) -> String {
        format!("one of {}", self.values.join(", "))
    }

    fn validate(self, key: &str) -> Result<(), SettingsError> {
        if self.values.is_empty() {
            return Err(SettingsError::RegistryInvariant(format!(
                "`{key}` enum settings must define at least one value"
            )));
        }

        let mut values = HashSet::new();
        for value in self.values {
            if value.is_empty() {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum settings must not define empty values"
                )));
            }

            if !values.insert(*value) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum setting has duplicate value `{value}`"
                )));
            }
        }

        let mut aliases = HashSet::new();
        for alias in self.aliases {
            if alias.from.is_empty() || alias.to.is_empty() {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum settings must not define empty aliases"
                )));
            }

            if values.contains(alias.from) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum alias `{}` duplicates a canonical value",
                    alias.from
                )));
            }

            if !values.contains(alias.to) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum alias `{}` points to unknown value `{}`",
                    alias.from, alias.to
                )));
            }

            if !aliases.insert(alias.from) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum setting has duplicate alias `{}`",
                    alias.from
                )));
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingAlias {
    from: &'static str,
    to: &'static str,
}

impl SettingAlias {
    pub fn from(self) -> &'static str {
        self.from
    }

    pub fn to(self) -> &'static str {
        self.to
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegerSettingType {
    min: i64,
    max: i64,
}

impl IntegerSettingType {
    pub fn min(self) -> i64 {
        self.min
    }

    pub fn max(self) -> i64 {
        self.max
    }

    pub fn contains(self, value: i64) -> bool {
        value >= self.min && value <= self.max
    }

    fn expected(self) -> String {
        format!("an integer from {} to {}", self.min, self.max)
    }

    fn validate(self, key: &str) -> Result<(), SettingsError> {
        if self.min <= self.max {
            Ok(())
        } else {
            Err(SettingsError::RegistryInvariant(format!(
                "`{key}` integer setting has an invalid range {}..{}",
                self.min, self.max
            )))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingValue {
    Enum(&'static str),
    Integer(i64),
}

impl SettingValue {
    pub fn as_enum(self) -> Option<&'static str> {
        match self {
            Self::Enum(value) => Some(value),
            Self::Integer(_) => None,
        }
    }

    pub fn as_integer(self) -> Option<i64> {
        match self {
            Self::Enum(_) => None,
            Self::Integer(value) => Some(value),
        }
    }
}

impl fmt::Display for SettingValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Enum(value) => write!(f, "{value}"),
            Self::Integer(value) => write!(f, "{value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingMutability {
    ReadOnly,
    ReadWrite,
}

impl SettingMutability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::ReadWrite => "read-write",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingOperation {
    Get,
    Describe,
    Set,
    Unset,
}

impl SettingOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Describe => "describe",
            Self::Set => "set",
            Self::Unset => "unset",
        }
    }
}

impl fmt::Display for SettingOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyStrategy {
    RestartUserScreenService,
    PendingLifecycleService,
}

impl ApplyStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RestartUserScreenService => "restart-user-screen-service",
            Self::PendingLifecycleService => "pending-lifecycle-service",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsError {
    InvalidKey {
        key: String,
        reason: &'static str,
    },
    UnknownKey(String),
    InvalidValue {
        key: String,
        value: String,
        expected: String,
    },
    UnsupportedOperation {
        key: String,
        operation: SettingOperation,
    },
    RegistryInvariant(String),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey { key, reason } => {
                write!(f, "invalid setting key `{key}`: {reason}")
            }
            Self::UnknownKey(key) => write!(f, "unknown setting `{key}`"),
            Self::InvalidValue {
                key,
                value,
                expected,
            } => write!(
                f,
                "invalid value for setting `{key}`: `{value}`; expected {expected}"
            ),
            Self::UnsupportedOperation { key, operation } => {
                write!(
                    f,
                    "setting `{key}` does not support `{}`",
                    operation.as_str()
                )
            }
            Self::RegistryInvariant(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for SettingsError {}

fn validate_setting_key(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("must not be empty");
    }

    let mut last_was_dot = true;
    let mut has_dot = false;

    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' | b'_' => {
                last_was_dot = false;
            }
            b'.' if !last_was_dot => {
                last_was_dot = true;
                has_dot = true;
            }
            b'.' => return Err("must not contain empty segments"),
            _ => {
                return Err(
                    "must contain only ASCII lowercase letters, digits, underscores, and dots",
                )
            }
        }
    }

    if last_was_dot {
        return Err("must not end with a dot");
    }

    if !has_dot {
        return Err("must contain at least one dot");
    }

    Ok(())
}

fn validate_storage_key(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("must not be empty");
    }

    if value
        .bytes()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
    {
        Ok(())
    } else {
        Err("must contain only ASCII lowercase letters, digits, and underscores")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ApplyStrategy, SettingKey, SettingMutability, SettingOperation, SettingType, SettingValue,
        SettingsError, SETTINGS_REGISTRY,
    };

    #[test]
    fn registry_metadata_is_internally_valid() {
        SETTINGS_REGISTRY.validate().unwrap();
    }

    #[test]
    fn registry_contains_initial_settings() {
        let keys: Vec<&str> = SETTINGS_REGISTRY
            .all()
            .iter()
            .map(|definition| definition.key_name())
            .collect();

        assert_eq!(
            keys,
            vec![
                "screen.backend",
                "screen.idle_timeout",
                "screen.restore_policy",
                "system.sleep_wake_policy",
            ]
        );
    }

    #[test]
    fn registry_maps_public_keys_to_storage_keys() {
        let mappings: Vec<(&str, &str)> = SETTINGS_REGISTRY
            .all()
            .iter()
            .map(|definition| (definition.key_name(), definition.storage_key()))
            .collect();

        assert_eq!(
            mappings,
            vec![
                ("screen.backend", "screen_backend"),
                ("screen.idle_timeout", "screen_idle_timeout"),
                ("screen.restore_policy", "screen_restore_policy"),
                ("system.sleep_wake_policy", "system_sleep_wake_policy"),
            ]
        );
    }

    #[test]
    fn key_parser_accepts_supported_dotted_names() {
        for key in [
            "screen.backend",
            "screen.idle_timeout",
            "system.sleep_wake_policy",
        ] {
            assert_eq!(SettingKey::parse(key).unwrap().as_str(), key);
        }
    }

    #[test]
    fn key_parser_rejects_invalid_names() {
        for key in [
            "",
            "screen",
            ".screen.backend",
            "screen.",
            "screen..backend",
            "Screen.backend",
            "screen-backend",
            "screen backend",
        ] {
            assert!(
                matches!(
                    SettingKey::parse(key),
                    Err(SettingsError::InvalidKey { .. })
                ),
                "expected invalid key for `{key}`"
            );
        }
    }

    #[test]
    fn lookup_returns_definitions_by_key() {
        let definition = SETTINGS_REGISTRY
            .get_by_name("screen.idle_timeout")
            .unwrap();

        assert_eq!(definition.key_name(), "screen.idle_timeout");
        assert_eq!(definition.storage_key(), "screen_idle_timeout");
        assert_eq!(definition.default_value(), SettingValue::Integer(300));
        assert_eq!(definition.mutability(), SettingMutability::ReadWrite);
    }

    #[test]
    fn lookup_rejects_unknown_keys() {
        assert!(matches!(
            SETTINGS_REGISTRY.get_by_name("screen.unknown"),
            Err(SettingsError::UnknownKey(key)) if key == "screen.unknown"
        ));
    }

    #[test]
    fn screen_backend_values_are_validated() {
        let definition = SETTINGS_REGISTRY.get_by_name("screen.backend").unwrap();

        for value in ["auto", "gnome", "swayidle"] {
            assert_eq!(definition.parse_value(value), Ok(SettingValue::Enum(value)));
        }

        assert!(matches!(
            definition.parse_value("kde"),
            Err(SettingsError::InvalidValue { .. })
        ));
    }

    #[test]
    fn screen_restore_policy_accepts_legacy_alias() {
        let definition = SETTINGS_REGISTRY
            .get_by_name("screen.restore_policy")
            .unwrap();

        assert_eq!(
            definition.parse_value("conservative"),
            Ok(SettingValue::Enum("conservative"))
        );
        assert_eq!(
            definition.parse_value("marker_only"),
            Ok(SettingValue::Enum("conservative"))
        );
        assert_eq!(
            definition.parse_value("aggressive"),
            Ok(SettingValue::Enum("aggressive"))
        );
    }

    #[test]
    fn integer_values_are_range_checked() {
        let definition = SETTINGS_REGISTRY
            .get_by_name("screen.idle_timeout")
            .unwrap();

        assert_eq!(definition.parse_value("1"), Ok(SettingValue::Integer(1)));
        assert_eq!(
            definition.parse_value("86400"),
            Ok(SettingValue::Integer(86_400))
        );

        for value in ["0", "86401", "-1", "abc"] {
            assert!(
                matches!(
                    definition.parse_value(value),
                    Err(SettingsError::InvalidValue { .. })
                ),
                "expected invalid idle timeout for `{value}`"
            );
        }
    }

    #[test]
    fn mutability_controls_supported_operations() {
        let screen_definition = SETTINGS_REGISTRY.get_by_name("screen.backend").unwrap();
        assert_eq!(
            screen_definition.supported_operations(),
            &[
                SettingOperation::Get,
                SettingOperation::Describe,
                SettingOperation::Set,
                SettingOperation::Unset,
            ]
        );
        screen_definition
            .ensure_operation_supported(SettingOperation::Set)
            .unwrap();

        let sleep_definition = SETTINGS_REGISTRY
            .get_by_name("system.sleep_wake_policy")
            .unwrap();
        assert_eq!(sleep_definition.mutability(), SettingMutability::ReadOnly);
        assert_eq!(
            sleep_definition.supported_operations(),
            &[SettingOperation::Get, SettingOperation::Describe]
        );
        assert_eq!(
            sleep_definition.ensure_operation_supported(SettingOperation::Set),
            Err(SettingsError::UnsupportedOperation {
                key: "system.sleep_wake_policy".to_string(),
                operation: SettingOperation::Set,
            })
        );
    }

    #[test]
    fn definitions_expose_type_and_apply_metadata() {
        let idle_timeout = SETTINGS_REGISTRY
            .get_by_name("screen.idle_timeout")
            .unwrap();
        assert!(matches!(idle_timeout.value_type(), SettingType::Integer(_)));
        assert_eq!(
            idle_timeout.apply_strategy(),
            ApplyStrategy::RestartUserScreenService
        );

        let sleep_policy = SETTINGS_REGISTRY
            .get_by_name("system.sleep_wake_policy")
            .unwrap();
        assert!(matches!(sleep_policy.value_type(), SettingType::Enum(_)));
        assert_eq!(
            sleep_policy.apply_strategy(),
            ApplyStrategy::PendingLifecycleService
        );
    }
}

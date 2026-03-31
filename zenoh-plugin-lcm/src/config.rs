use regex::Regex;
use serde::{
    de, de::Visitor, Deserialize, Deserializer, Serialize, Serializer,
};

const DEFAULT_LCM_URL: &str = "udpm://239.255.76.67:7667?ttl=0";
const DEFAULT_KEY_PREFIX: &str = "lcm";
const DEFAULT_MAX_MESSAGE_SIZE: usize = 4 * 1024 * 1024; // 4 MB
pub const DEFAULT_WORK_THREAD_NUM: usize = 2;
pub const DEFAULT_MAX_BLOCK_THREAD_NUM: usize = 50;

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_lcm_url")]
    pub lcm_url: String,

    #[serde(default = "default_key_prefix")]
    pub key_prefix: String,

    #[serde(
        default,
        deserialize_with = "deserialize_regex",
        serialize_with = "serialize_allow"
    )]
    pub allow: Option<Regex>,

    #[serde(
        default,
        deserialize_with = "deserialize_regex",
        serialize_with = "serialize_deny"
    )]
    pub deny: Option<Regex>,

    #[serde(default)]
    pub network_interface: Option<String>,

    #[serde(default = "default_max_message_size")]
    pub max_message_size: usize,

    #[serde(default = "default_work_thread_num")]
    pub work_thread_num: usize,

    #[serde(default = "default_max_block_thread_num")]
    pub max_block_thread_num: usize,

    __required__: Option<bool>,

    #[serde(default, deserialize_with = "deserialize_path")]
    __path__: Option<Vec<String>>,
}

fn default_lcm_url() -> String {
    DEFAULT_LCM_URL.to_string()
}

fn default_key_prefix() -> String {
    DEFAULT_KEY_PREFIX.to_string()
}

fn default_max_message_size() -> usize {
    DEFAULT_MAX_MESSAGE_SIZE
}

fn default_work_thread_num() -> usize {
    DEFAULT_WORK_THREAD_NUM
}

fn default_max_block_thread_num() -> usize {
    DEFAULT_MAX_BLOCK_THREAD_NUM
}

// --- Custom deserializers (following zenoh-plugin-mqtt conventions) ---

fn deserialize_regex<'de, D>(deserializer: D) -> Result<Option<Regex>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Deserialize::deserialize(deserializer)?;
    match s {
        Some(s) => Regex::new(&s).map(Some).map_err(|e| {
            de::Error::custom(format!(
                r#"Invalid regex for 'allow' or 'deny': "{s}" - {e}"#
            ))
        }),
        None => Ok(None),
    }
}

fn serialize_allow<S>(v: &Option<Regex>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(
        &v.as_ref()
            .map_or_else(|| ".*".to_string(), |re| re.to_string()),
    )
}

fn serialize_deny<S>(v: &Option<Regex>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(
        &v.as_ref()
            .map_or_else(|| "".to_string(), |re| re.to_string()),
    )
}

fn deserialize_path<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_option(OptPathVisitor)
}

struct OptPathVisitor;

impl<'de> Visitor<'de> for OptPathVisitor {
    type Value = Option<Vec<String>>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "none or a string or an array of strings")
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(PathVisitor).map(Some)
    }
}

struct PathVisitor;

impl<'de> Visitor<'de> for PathVisitor {
    type Value = Vec<String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "a string or an array of strings")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(vec![v.into()])
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let mut v = if let Some(l) = seq.size_hint() {
            Vec::with_capacity(l)
        } else {
            Vec::new()
        };
        while let Some(s) = seq.next_element()? {
            v.push(s);
        }
        Ok(v)
    }
}

/// Check if an LCM channel name is allowed by the allow/deny configuration.
pub fn is_allowed(channel: &str, config: &Config) -> bool {
    match (&config.allow, &config.deny) {
        (Some(allow), None) => allow.is_match(channel),
        (None, Some(deny)) => !deny.is_match(channel),
        (Some(allow), Some(deny)) => allow.is_match(channel) && !deny.is_match(channel),
        (None, None) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(config.lcm_url, DEFAULT_LCM_URL);
        assert_eq!(config.key_prefix, DEFAULT_KEY_PREFIX);
        assert!(config.allow.is_none());
        assert!(config.deny.is_none());
        assert!(config.network_interface.is_none());
    }

    #[test]
    fn test_allow_deny() {
        let config: Config =
            serde_json::from_str(r#"{"allow": "SENSOR_.*", "deny": "SENSOR_DEBUG"}"#).unwrap();
        assert!(is_allowed("SENSOR_IMU", &config));
        assert!(!is_allowed("SENSOR_DEBUG", &config));
        assert!(!is_allowed("MOTOR_CMD", &config));
    }

    #[test]
    fn test_path_field() {
        let config: Config =
            serde_json::from_str(r#"{"__path__": "/example/path"}"#).unwrap();
        assert!(config.__path__.is_some());
    }

    #[test]
    fn test_path_field_as_array() {
        let config: Config =
            serde_json::from_str(r#"{"__path__": ["/a", "/b"]}"#).unwrap();
        let paths = config.__path__.unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], "/a");
    }

    #[test]
    fn test_invalid_regex_rejected() {
        let result: Result<Config, _> =
            serde_json::from_str(r#"{"allow": "[invalid"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_deny_only() {
        let config: Config =
            serde_json::from_str(r#"{"deny": "DEBUG_.*"}"#).unwrap();
        assert!(is_allowed("SENSOR_IMU", &config));
        assert!(!is_allowed("DEBUG_TEMP", &config));
    }

    #[test]
    fn test_allow_only() {
        let config: Config =
            serde_json::from_str(r#"{"allow": "SENSOR_.*"}"#).unwrap();
        assert!(is_allowed("SENSOR_IMU", &config));
        assert!(!is_allowed("MOTOR_CMD", &config));
    }

    #[test]
    fn test_neither_allow_nor_deny() {
        let config: Config = serde_json::from_str("{}").unwrap();
        assert!(is_allowed("ANYTHING", &config));
        assert!(is_allowed("", &config));
    }

    #[test]
    fn test_allow_and_deny_overlap() {
        // Allow SENSOR_.*, deny SENSOR_DEBUG.* — SENSOR_DEBUG_X should be denied.
        let config: Config =
            serde_json::from_str(r#"{"allow": "SENSOR_.*", "deny": "SENSOR_DEBUG.*"}"#).unwrap();
        assert!(is_allowed("SENSOR_IMU", &config));
        assert!(!is_allowed("SENSOR_DEBUG_X", &config));
        assert!(!is_allowed("MOTOR_CMD", &config));
    }

    #[test]
    fn test_unknown_field_rejected() {
        let result: Result<Config, _> =
            serde_json::from_str(r#"{"unknown_field": true}"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_custom_thread_settings() {
        let config: Config =
            serde_json::from_str(r#"{"work_thread_num": 4, "max_block_thread_num": 100}"#).unwrap();
        assert_eq!(config.work_thread_num, 4);
        assert_eq!(config.max_block_thread_num, 100);
    }

    #[test]
    fn test_config_serialization_roundtrip() {
        let config: Config =
            serde_json::from_str(r#"{"allow": "SENSOR_.*", "deny": "DEBUG_.*"}"#).unwrap();
        let json = serde_json::to_value(&config).unwrap();
        // Serialized allow should show the regex pattern, deny likewise.
        assert_eq!(json["allow"], "SENSOR_.*");
        assert_eq!(json["deny"], "DEBUG_.*");
    }
}
